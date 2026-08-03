# Oracle source 锚点提取例外

配置位于 `anchor-none.yaml`，粒度为 oracle `id` 加单个 `identifier`。这里只允许登记提取器误识别出的普通词；同一普通词累计出现三次时必须改进 `extractStatementIdentifiers`，不得继续增加登记。

当前登记数：**0 个标识符**。因此每个可提取标识符都继续接受 `source-anchor` 检查。

另有 218 个 statement 没有可提取标识符；它们不是例外项，也不进入递减基线。机器规则会在 statement 将来出现代码形态标识符时自动开始检查。
