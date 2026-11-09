# `.geoproj` schema 变更记录

版本号与 `23 §3.2` 对齐：JSON Schema 只表**主版本**，`v1.json` 对应 `schema_version = 1`。

## v1（未发布）

- 初版，覆盖 `23 §3.4` 的全部字段：文档引用与指纹、模板引用、处理器图、YARA、
  断言、书签、搜索历史、AI provenance。
- `processors.nodes[].params` 与 `search_history.queries[]` 用 `additionalProperties: true`
  放开：前者由 `geode-proc` 各节点自校验，后者只存查询串与参数。
