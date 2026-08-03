# P8b-1 变异表

私有 runner：`scratchpad/mut-p8b1.sh`（按 brief 保持 gitignored）。每次 `mutate()` 都先保留原文件，变异后用 `cmp -s` 验证目标确实变化；未变化立即退出。表中列出全量变红用例，不用 `-t` 人为制造“恰好一个”。

| 变异 | 被削弱的实现 | 全量变红用例 | 说明 |
|---|---|---|---|
| source-anchor unconditional accept | statement identifier 不再约束引用区间 | `source-anchor: accepts positive and rejects only the intended negative` | negative fixture 的标识符仅在未引用第 2 行，规则失效后静默通过 |
| former-id-format unconditional accept | 非字符串或当前 id 可冒充 retired id | `field type former_id: accepts positive and rejects only former-id-format`; `former-id-format: accepts positive and rejects only the intended negative` | 字段类型 negative 变 0 条；同 id negative 错落到 unique，证明 format 分支失效 |
| former-id-unique current collision bypass | retired id 可复用 current id | `former-id-unique: accepts positive and rejects only the intended negative` | negative fixture 的 former/current 碰撞不再报告 |
