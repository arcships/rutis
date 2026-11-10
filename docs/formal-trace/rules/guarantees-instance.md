# 规则：`instance/*`（保证登记表 §9）

[中文索引](../README.md) · [English](../README.en.md) · 数据来源：[保证登记表](../../guarantees.md)

由 `node tools/formal-trace.mjs --write` 从登记表生成：自动列（编号、现行来源、人话承诺、级别、证据、状态）不要手改，手改会被 `--check` 报出；人工列（适用范围、变异或反例、缺口）按编号保留。缺口的初始值取登记表的「说明」列。

编号沿用登记表，一经登记不复用；撤销的行保留。

| 编号 | 现行来源 | 人话承诺 | 级别 | 适用范围 | 证据 | 变异或反例 | 状态 | 缺口 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `instance/subtrees.2.2` | [guarantees.md](../../guarantees.md) §9 | 实例 ID 在创建插件时分配，进程内唯一；同一插件 restart / update / 依赖重载不改变它，重新建立的子插件得到新 ID；耗尽时以明确原因 panic，不回绕；保留的旧 `Ctx` 仍报告旧 ID，旧 ID 读不到新插件的服务 | 契约行为 | 未登记 | `rutis/src/key.rs::instance_id_exhaustion_panics_instead_of_wrapping`、`rutis/tests/instance_subtrees.rs::a_recreated_child_gets_a_new_instance_id`、`::fiber_instance_stays_stable_across_restart_update_and_dependency_reload` | 未登记 | 已验证 | 耗尽的 panic 发生在持有 root 的准入锁时，会让这把锁中毒（u64 实际不可达，只记录；见 #286） |
| `instance/subtrees.5` | [guarantees.md](../../guarantees.md) §9 | 长寿 root 上反复创建和关闭子树（包括旁边有常驻兄弟）之后不留残余：插件被释放，内部表项、任务数回到基线，父级表的容量不随轮数增长，常驻兄弟不重载 | 核心承诺 | 未登记 | `rutis/tests/instance_subtrees.rs::thousand_subtrees_next_to_a_resident_leave_no_tasks_behind`、`::thousand_subtrees_leave_no_live_fibers`、`rutis/src/fiber/transient_tests.rs::parent_tables_shrink_back_after_every_batch_of_subtrees`、`::thousand_shutdowns_reclaim_all_root_side_records`、`rutis/tests/transient_release.rs::churn_on_long_lived_root_releases_instances` | 未登记 | 已验证 | 结束后不留残余（Q3.1）。单个键下的消费者 / 监听器列表保留峰值容量（不随轮数增长，不违反 §4.6；见 #286） |
