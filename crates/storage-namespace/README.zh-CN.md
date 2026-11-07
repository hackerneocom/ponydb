[English](README.md) | **中文**

# storage-namespace

布局 v1 的路径规则。数据库里的每条持久引用都是相对根的键，把这些键变成对象名的逻辑只在这个 crate 里。

## 用途

对象存储是扁平的键空间。ponydb 把多个数据库放在同一个桶前缀下，元数据又在每个数据库内部保存更深的键。这些键必须留在该数据库之内：引用不能借助 `..`、绝对路径，或与邻居重叠的前缀（例如 `notes` 与 `notes_old`）逃逸出去。

## 工作原理

`DatabaseName` 校验用户指定、不可修改的逻辑名称：1–64 个 Unicode BMP 字符，禁止 NUL、`/`、`\\`、`.`、补充平面字符和末尾 ASCII 空格。名称区分大小写并原样保存。`IncarnationId` 校验一个非空 ASCII 相对路径段，表示创建或恢复代际，不要求 UUID 格式。

`RelativeKey` 校验数据库根相对引用。每段非空且不能是 `.` 或 `..`，字节只允许 ASCII 字母数字及 `/-_.`；拒绝绝对路径、反斜杠、百分号编码、URL scheme 和末尾斜杠。`head.json` 等一般安全相对键不必符合不可变对象布局。

`StorageNamespace` 保存可选的已验证前缀，通过 `database_name` 解析数据库列表候选。`database(DatabaseName)` 创建绑定单库完整根前缀（含末尾 `/`）的 `DatabasePath`。运行时使用空命名空间，仅由 S3 适配器追加一次配置的部署前缀。

| 方法 | 对象名 |
| --- | --- |
| `StorageNamespace::databases_prefix` | `{prefix}databases/` |
| `DatabasePath::prefix` | `{prefix}databases/{name}/` |
| `DatabasePath::head_key` | `{prefix}databases/{name}/head.json` |
| `DatabasePath::incarnations_prefix` | `{prefix}databases/{name}/incarnations/` |
| `DatabasePath::resolve` | `{prefix}databases/{name}/{relative-key}` |

`DatabasePath::relative_key` 只剥离当前库的完整根前缀，再校验余下引用；绑定 `notes` 时拒绝来自 `notes_old` 的对象。

`object_key` 构造 `incarnations/{incarnation}/generations/{epoch:016x}/{category}/{uuid}`；`RelativeKey::object_parts` 将同一 v1 布局解析为代际、正数 epoch、category 和对象后缀。epoch 固定为 16 位小写十六进制。GC 拒绝未知布局。布局 v1 接受安全的嵌套对象后缀。

该 crate 不执行存储 I/O。黄金路径测试验证布局 v1 的路径格式。
