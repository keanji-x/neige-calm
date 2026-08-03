# P8b-1 变异表

私有 runner：`scratchpad/mut-p8b1.sh`（按 brief 保持 gitignored）。每次 `mutate()` 都先保留原文件，变异后用 `cmp -s` 验证目标确实变化；未变化立即退出。表中列出全量变红用例，不用 `-t` 人为制造“恰好一个”。

完整实跑：6/6 变异均非零退出；前五组各 1 个失败用例，最后一组 3 个。脚本退出码均为 1（变异脚本自身总退出码 0）。

| 变异 | 被削弱的实现 | 全量变红用例 | exit / 数量 |
|---|---|---|---|
| source-anchor unconditional accept | 所有锚点违规被接受 | `source-anchor: accepts positive and rejects only the intended negative` | 1 / 1 |
| identifier boundary disabled | `useAnchor` 可被 `useAnchoredValue` 满足 | `source-anchor: accepts positive and rejects only the intended negative` | 1 / 1 |
| comments count as code | `// commentOnlyAnchor` 可满足锚点 | `source-anchor: accepts positive and rejects only the intended negative` | 1 / 1 |
| missing identifier silent | 标识符在被引文件不存在时静默通过 | `source-anchor: accepts positive and rejects only the intended negative` | 1 / 1 |
| duplicate retired handle bypass | 两条 `former_id` 可互相重复 | `former-id-unique: accepts positive and rejects only the intended negative` | 1 / 1 |
| former-id unconditional accept | format/碰撞/重复分支全部失效 | `field type former_id: accepts positive and rejects only former-id-format`; `former-id-format: accepts positive and rejects only the intended negative`; `former-id-unique: accepts positive and rejects only the intended negative` | 1 / 3 |
