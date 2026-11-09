# `.geows` schema 变更记录

版本号与 `23 §3.2` 对齐：JSON Schema 只表**主版本**，`v1.json` 对应 `schema_version = 1`。

## v1（未发布）

- 初版，覆盖 `23 §3.3` 的全部字段：`schema_version` / `kind` / `workspace_id` / `project` /
  `window` / `layout` / `docs[]` / `views[]`。
- `layout` 因禁用 `$ref` 只能表达一层：**树深 ≤16、节点 ≤256 由加载器校验（EB005）**，
  schema 不重复表达这条约束（避免两处漂移）。
