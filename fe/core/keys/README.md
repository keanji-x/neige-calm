# core/keys

## 用途

冻结持久化 key 的唯一出处、动态 key 工厂和带 brand 的 storage adapter port。集中定义是因为登出、缓存爆破与事件恢复必须操作完全相同的 key。

## 契约

三个已部署 key 保持逐字符稳定，因为改名会造成用户状态静默丢失。动态 key 统一使用 `calm:` 加冒号分段，因为调用方不得各自拼接命名空间；空段或含冒号的段会同步拒绝，因为否则会产生歧义 key。adapter 只接受 `StorageKey` 并沿用 core/state 的显式结果通道，因为平台异常与配额失败不能泄漏成未约定的 throw。

## 故意不做

这里不访问 localStorage/IndexedDB、不选择 codec、不实现迁移或清理策略，因为平台和生命周期策略由端侧注入；也不为任意旧 key 提供逃生字符串转换，避免重新制造多出处。
