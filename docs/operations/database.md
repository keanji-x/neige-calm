# 数据库定位与读取

## 找到当前实例实际使用的数据库

1. 从服务启动命令确认配置文件和命令行覆盖项。
2. 查看有效的 `child.db_url` 与 `child.data_dir`。省略 DB URL 时，neige-app 按数据目录生成 `sqlite://<data_dir>/calm.db?mode=rwc`。显式配置可能指定其他路径或开发用的 `mock`。
3. 对相对路径核对服务工作目录，并确认目标文件存在、与实例相符。把解析出的绝对路径记入本次排障记录。

配置来源和覆盖顺序见 [配置指南](../neige-app-config.md)。示例里的默认目录只能帮助理解布局，实际操作使用目标实例的有效配置。

## 文件格式与读取方式

持久化数据库使用 SQLite。WAL 模式下，主文件旁可能同时存在 `-wal` 和 `-shm` 文件；主文件的修改时间不足以说明最近有没有写入。备份与恢复使用 [部署指南的数据库备份章节](../deploy-and-upgrade.md#81-database-backup-and-manual-restore-read-this-before-allowbreaking)。

优先通过产品的只读接口获取领域状态。需要检查数据库时，用实际路径打开只读连接：

```sh
sqlite3 -readonly /absolute/path/to/calm.db
```

先发现当前 schema：

```sql
SELECT sqlite_version();
PRAGMA database_list;
PRAGMA journal_mode;
.tables
.schema
```

`.tables` 和 `.schema` 是 sqlite3 CLI 命令。针对选定对象再用 `.schema <table_name>` 查看列、索引和约束，按稳定 ID 做有界查询。SQLx 管理的部署可查看 `_sqlx_migrations` 的实际结构及迁移记录，并与运行版本核对。

表、列和状态枚举随版本演进。查询前核对当前 schema 与状态读取实现，优先返回所需列并限制结果数。跨表关联需要确认所属实例以及 Track、attempt、session 等身份关系，单行状态只能说明该行的事实。

## 怎样判断数据库状态

- 写入等待与重试先看 [现有事务观测](../db-transaction-observation.md)，明确采样范围和统计口径。
- 库或 WAL 增长时，结合磁盘余量、事务活动与留存策略判断。
- 完整性检查、checkpoint、压缩和恢复属于单独的维护步骤，应按对应 runbook 的运行条件执行。
- 观测缺失时记录为未知。重启后的内存指标需要重新积累，查询失败保留原始错误与发生时间。

记录中保留实例、版本、查询时间、查询范围和必要的标识，敏感业务内容与凭据做脱敏处理。
