# P8b-1 变异表

Runner：`scratchpad/mut-p8b1.sh`。每次 `replace()` 都先保留原文件，变异后用 `cmp -s` 验证目标确实变化；未变化立即退出。表中使用全量测试，不用 `-t` 人为制造“恰好一个”。

完整实跑：9/9 变异下测试均 exit 1；runner 总 exit 0。

| 变异 | 被削弱的实现 | 全量变红用例 | exit / 数量 |
|---|---|---|---|
| source-anchor unconditional accept | 所有锚点违规被接受 | fixture、baseline 双向、prose、真数据 | 1 / 4 |
| identifier boundary disabled | `useAnchor` 可被 `useAnchoredValue` 满足 | fixture、baseline 双向、prose、真数据 | 1 / 4 |
| comments count as code | `// commentOnlyAnchor` 可满足锚点 | fixture、baseline 双向、prose、真数据 | 1 / 4 |
| missing identifier silent | 标识符在被引文件不存在时静默通过 | baseline 双向、真数据 | 1 / 2 |
| duplicate retired handle bypass | 两条 `former_id` 可互相重复 | `former-id-unique: accepts positive and rejects only the intended negative` | 1 / 1 |
| former-id unconditional accept | format/碰撞/重复分支全部失效 | `field type former_id: accepts positive and rejects only former-id-format`; `former-id-format: accepts positive and rejects only the intended negative`; `former-id-unique: accepts positive and rejects only the intended negative` | 1 / 3 |
| baseline unbaselined loop disabled | 新违规不再报 `unbaselined` | fixture、baseline 双向、prose | 1 / 3 |
| baseline stale loop disabled | 已修债务仍留在基线 | baseline 双向 | 1 / 1 |
| baseline count guard disabled | 重复行或数量不等不再报错 | baseline 双向 | 1 / 1 |
| duplicate unsupported id | unsupported 台账重复 id 被静默覆盖 | negative fixture、真数据 | 1 / 2 |
