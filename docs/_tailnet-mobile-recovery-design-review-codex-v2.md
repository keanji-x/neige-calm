**APPROVE**

已完整读取 258 行 v2 与上轮复核，SHA256 与指定值一致；补读相关源码共 6 次。

1. **A-1/B-1 决议成立，可关闭。** 排队前准入、不可更新的代际许可、发送前复核及旧响应/401 验代，已覆盖延迟 mutation、串行队列和终端发送的反例；恢复后禁止续发旧意图。[设计:130](/tmp/neige-recovery-baseline/docs/architecture/tailnet-mobile-recovery.md:130)

2. **A-2/B-2 决议成立，可关闭。** whoami 确实已有必需的 sessionId，“缺少该字段”的判断不成立。[auth.rs:334](/tmp/neige-recovery-baseline/crates/calm-server/src/auth.rs:334) 指纹拒绝标记、用户主动验证不同 session、成功清标记后才进入授权流程，且失败/取消保留阻断，已形成兼容旧配对入口的闭环。[设计:100](/tmp/neige-recovery-baseline/docs/architecture/tailnet-mobile-recovery.md:100)

3. **A-3/B-3 决议成立，可关闭。** HTTPS 名称、SNI、证书、peer、端口与受信节点地址绑定明确；二维码仅提议目标，绑定变化需重新确认，已补齐可配置目标的信任边界。[设计:183](/tmp/neige-recovery-baseline/docs/architecture/tailnet-mobile-recovery.md:183)

完整设计未发现新的阻塞矛盾：冷启动无正文呈现、鉴权与版本门禁、唯一恢复 owner、受限 ingress 及单 writer 回滚约束可以共同成立。具体接线与竞态验证留待实现评审，无需在设计中展开完整实现。

本次仅批准设计；未运行测试、联网、修改文件或派 agent。