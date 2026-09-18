REQUEST_CHANGES

1. **P1：扫码许可跨文档传递尚未闭合。** [设计第102行](/tmp/neige-scan-review-b/docs/architecture/1712-scan-only-enrollment.md:102)只规定向配对页注入内存中的 scan attempt；[第123–124行](/tmp/neige-scan-review-b/docs/architecture/1712-scan-only-enrollment.md:123)随后要求 `/next/` 的 bundled gate 核验仍有效的 native generation。完整导航会销毁配对页 JS 内存，sessionStorage 中的 receipt 不能证明原生许可仍有效；现有 [BundledWebViewClient](/tmp/neige-scan-review-b/mobile/src-tauri/gen/android/app/src/main/java/io/neigecalm/next/BundledWebViewClient.kt:31)也没有该交接。实现因此必须自行选择卡住自动验证，或弱化为信任网页存储。最小修订：明确 native 保留此次 attempt，仅在允许的配对页→bundled 页面跳转时向目标顶层文档单向交付许可，并定义取消、超时及其他导航的失效规则；无需开放远端 IPC。

其余重点未发现必须另开方案的阻塞：短 expiry 与签发未知结果已明确失败关闭；已有身份保护、同 attempt 单 session 重兑及 receipt/cookie/退出指纹绑定已有合同。300 秒云端寿命仍是发布前实测条件。

已核对指定提交、165 行及 SHA；全程只读，未运行测试、网络或变异。