# core/state

## 用途

冻结跨端持久状态的基础设施类型：phantom brand、codec、显式结果通道和异步 storage port。集中在 `types.ts` 是因为它是 UI 层获准依赖的唯一状态白名单入口。

## 契约

`Persistent<T>` 只存在于编译期，因为运行时包装会改变数据和值身份。codec 把解码失败作为结果返回，因为损坏数据必须与“没有数据”区分。storage port 把读取失败、解码失败、普通写失败和配额超限显式分类，因为调用方需要分别决定保留旧值、采用默认值或提示用户；所有平台访问由 adapter 注入，保证 core 可在 Node 中测试。overlay 状态固定为 `[Persistent<T>, setter]`，因为它必须与本地 state 的消费形状对称且不能凭空增加 loading 分支。overlay key 固定为五元组，因为持久化与失效都依赖同一 key 族；同步更新返回含 previous/next 的逐调用 mutation，再把它传给 persist/rollback，因为乐观写必须立即可见且并发失败不能串用快照。

## 故意不做

这里不选择浏览器存储、不访问平台 API、不实现缓存、重试、迁移或 React hook，因为这些决定分别属于端侧 adapter 和 UI；也不实现 React Query、网络请求或具体 overlay 业务字段，因为阶段 1 只冻结泛型 port 与生命周期语义。
