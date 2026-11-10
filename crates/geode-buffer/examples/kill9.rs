//! S2 spike：journal 在 `kill -9` 下的丢帧窗口。
//!
//! 父进程反复把**自身**拉起来当子进程：子进程循环 `append`，父进程睡一个随机
//! 时长后 `SIGKILL`，再读回 journal 与 progress 文件，统计：
//!
//! - 恢复成功率（`JournalReader::open` 返回 `Ok`）；
//! - 最多丢几帧（progress 里最后一条完整记录 − journal 恢复出的 `last_seq`）；
//! - `Partial` 比例（尾部有半帧/坏帧的次数）。
//!
//! progress 文件每 append 一次就追加一行 seq（不 fsync），它是"用户以为已经
//! 提交"的基准；journal 里的帧可能还在 `BufWriter` 里，所以两者之差就是丢失。
//! 注意 progress 只用于对照：若 kill 落在 append 返回之后、progress 写入之前，
//! journal 会**领先** progress 一条，这不是丢失。
//!
//! 用法（在仓库根目录）：
//!
//! ```text
//! cargo run -q -p geode-buffer --example kill9 -- 100
//! ```

use geode_buffer::journal::{BaseKind, JournalHeader, JournalReader, JournalWriter};
use geode_core::Offset;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_RUNS: u64 = 100;
const PAYLOAD: [u8; 64] = [b'x'; 64];

fn main() {
    let mut args = env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("--child") {
        let journal = args.next().expect("--child 需要 journal 路径");
        let progress = args.next().expect("--child 需要 progress 路径");
        let interval_ms: u64 = args
            .next()
            .expect("--child 需要 interval")
            .parse()
            .expect("interval 必须是毫秒数");
        child(Path::new(&journal), Path::new(&progress), interval_ms);
    }
    let runs: u64 = first
        .map(|s| s.parse().expect("runs 必须是整数"))
        .unwrap_or(DEFAULT_RUNS);
    parent(runs);
}

/// 子进程：一直 append，直到被父进程 kill。
fn child(journal: &Path, progress: &Path, interval_ms: u64) -> ! {
    let header = JournalHeader::new([0x5A; 16], BaseKind::File, 0, 0, [0; 32]);
    let mut writer = JournalWriter::create(journal, header).expect("创建 journal");
    let mut progress = OpenOptions::new()
        .create(true)
        .append(true)
        .open(progress)
        .expect("打开 progress");
    let interval = Duration::from_millis(interval_ms);
    let mut i = 0u64;
    loop {
        writer
            .append_insert(Offset::new(i * PAYLOAD.len() as u64), &PAYLOAD)
            .expect("append");
        // 每行一个 seq，不 fsync：只保证对同机进程可见（page cache）。
        writeln!(progress, "{}", writer.seq()).expect("写 progress");
        i += 1;
        if !interval.is_zero() {
            std::thread::sleep(interval);
        }
    }
}

/// 单次 kill 的结果。
struct Run {
    interval_ms: u64,
    appended: u64,
    recovered: u64,
    ok: bool,
    partial: bool,
    code: Option<&'static str>,
    detail: Option<String>,
}

fn parent(runs: u64) {
    let root = env::temp_dir().join(format!("geode-kill9-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("创建临时目录");
    let exe = env::current_exe().expect("定位自身可执行文件");
    let mut rng = seed();
    let fast_runs = runs / 2;
    let mut results = Vec::with_capacity(runs as usize);

    for run in 0..runs {
        let fast = run < fast_runs;
        // 快档：不停顿地写，测 BufWriter 的 64 帧窗口；
        // 慢档：每 2 ms 一帧，测 1 秒时间窗口会不会先触发。
        let interval_ms = if fast { 0 } else { 2 };
        let delay_ms = if fast {
            30 + next_rand(&mut rng) % 370
        } else {
            100 + next_rand(&mut rng) % 1400
        };
        let dir = root.join(format!("run-{run:03}"));
        fs::create_dir_all(&dir).expect("创建 run 目录");
        let journal = dir.join("doc.geojournal");
        let progress = dir.join("progress");

        let mut command = Command::new(&exe);
        command
            .arg("--child")
            .arg(&journal)
            .arg(&progress)
            .arg(interval_ms.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("启动子进程");
        std::thread::sleep(Duration::from_millis(delay_ms));
        let _ = child.kill(); // Unix 上是 SIGKILL
        let _ = child.wait();

        // 等页缓存里的写落定（kill 不会丢已完成的 write(2)）。
        std::thread::sleep(Duration::from_millis(5));
        let appended = last_progress(&progress).unwrap_or(0);
        let result = match JournalReader::open(&journal) {
            Ok(recovered) => Run {
                interval_ms,
                appended,
                recovered: recovered.last_seq,
                ok: true,
                partial: recovered.partial,
                code: recovered.failure_code(),
                detail: None,
            },
            Err(error) => Run {
                interval_ms,
                appended,
                recovered: 0,
                ok: false,
                partial: false,
                code: error.code(),
                detail: Some(error.to_string()),
            },
        };
        results.push(result);
        let _ = fs::remove_dir_all(&dir);
    }

    report(&results);
    let _ = fs::remove_dir_all(&root);
}

/// 读 progress 里最后一条**带换行**的完整 seq；尾部半行忽略。
fn last_progress(path: &Path) -> Option<u64> {
    let text = fs::read_to_string(path).ok()?;
    text.split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .filter_map(|line| line.trim_end().parse::<u64>().ok())
        .next_back()
}

fn report(results: &[Run]) {
    let total = results.len();
    let ok = results.iter().filter(|r| r.ok).count();
    let partial = results.iter().filter(|r| r.partial).count();
    println!(
        "S2 journal kill -9 实验：{} 次（快档 {} / 慢档 {}）",
        total,
        results.iter().filter(|r| r.interval_ms == 0).count(),
        results.iter().filter(|r| r.interval_ms != 0).count()
    );
    println!(
        "恢复成功：{}/{}（{:.1}%）；Partial：{}（{:.1}%）",
        ok,
        total,
        100.0 * ok as f64 / total as f64,
        partial,
        100.0 * partial as f64 / total as f64
    );

    println!();
    println!("| 档位 | 次数 | 恢复成功 | Partial | 最多丢帧 | 平均丢帧 |");
    println!("|---|---:|---:|---:|---:|---:|");
    for (label, want_fast) in [("快档（不 sleep）", true), ("慢档（2 ms/帧）", false)] {
        let group: Vec<&Run> = results
            .iter()
            .filter(|r| (r.interval_ms == 0) == want_fast)
            .collect();
        if group.is_empty() {
            continue;
        }
        // journal 领先 progress 说明 kill 落在 append 返回后、progress 写入前，
        // 不是丢失；只有 progress 领先 journal 才计入丢帧。
        let lost: Vec<u64> = group
            .iter()
            .map(|r| r.appended.saturating_sub(r.recovered))
            .collect();
        let max = lost.iter().copied().max().unwrap_or(0);
        let avg = lost.iter().sum::<u64>() as f64 / lost.len() as f64;
        println!(
            "| {} | {} | {} | {} | {} | {:.2} |",
            label,
            group.len(),
            group.iter().filter(|r| r.ok).count(),
            group.iter().filter(|r| r.partial).count(),
            max,
            avg
        );
    }

    let max_lost = results
        .iter()
        .map(|r| r.appended.saturating_sub(r.recovered))
        .max()
        .unwrap_or(0);
    let max_lost_ms = results
        .iter()
        .map(|r| r.appended.saturating_sub(r.recovered) * r.interval_ms)
        .max()
        .unwrap_or(0);
    println!();
    println!("最多丢帧：{max_lost}；慢档按间隔折算最多丢 {max_lost_ms} ms 的编辑。");
    let mut codes: Vec<&'static str> = results.iter().filter_map(|r| r.code).collect();
    codes.sort_unstable();
    codes.dedup();
    println!("出现过的 failure code：{codes:?}");
    for run in results.iter().filter(|r| !r.ok) {
        println!(
            "失败样本：interval={} appended={} code={:?} detail={:?}",
            run.interval_ms, run.appended, run.code, run.detail
        );
    }
}

fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("时钟早于 epoch")
        .as_nanos() as u64;
    nanos ^ (u64::from(std::process::id()) << 32) ^ 0x9E37_79B9_7F4A_7C15
}

/// xorshift64*：够随机，且不引依赖。
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}
