APPROVE

旧阻塞已闭合：[同文档方案](/tmp/neige-scan-review-b3-final/docs/architecture/1712-scan-only-enrollment.md:112)通过 native 一次性交付 context，让 FE 完成 claim→幂等 redeem→whoami→version/scope，无须跨 remote 页面传递实时 generation，也不依赖 CONNECT 观察 TLS 明文。

[会话绑定与验证](/tmp/neige-scan-review-b3-final/docs/architecture/1712-scan-only-enrollment.md:129)已明确 attempt 匹配、新 session 指纹、实际 cookie 的 whoami 比对及退出标记清除顺序。取消由 FE abort/增代和 native 关闭实例连接、销毁文档承担；已采纳成功与已签 cookie 不承诺回滚，边界一致。v1 保持原审批流程。

全文未发现新阻塞，设计细节足以交付实现；300 秒真实寿命仍是文档明确的发布前实测条件。

已核对指定提交 `7a8563a544c665b61eab0a7e20cfa122e040ad1a`、184 行全文及旧评审；工作区干净。全程只读，未测试、联网、改文件或派 agent。