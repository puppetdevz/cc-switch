# 云同步内容分项选择与远端数据管理需求及技术实施文档

**文档版本**：1.0  
**日期**：2026-09-05  
**目标版本**：待 Harness 实施时确定  
**需求质量评分**：98/100  
**适用范围**：设置 → 高级 → 云同步；S3 与 WebDAV  

## 给 Harness 的执行指令

请把本文作为实现合同，而不是讨论稿。实施前先阅读仓库根目录的 `AGENTS.md`，并以当前代码为准复核本文引用的文件和符号。发现实现已经变化时，应保持本文确定的产品语义，并在交付说明中记录必要的技术调整。

需要完成前端、Rust 服务、同步协议、迁移兼容、测试和用户文档。不得只制作界面或只过滤上传请求。所有勾选项必须真正控制导出、上传、下载、恢复、自动同步、状态统计和远端删除。

发现缺陷时先增加能够稳定复现问题的测试，再修复。完成后运行本文列出的验证。没有执行真实 S3/WebDAV 验收时，不得把模拟传输测试描述为真实云同步验收。默认只在当前工作区实现，不提交、不推送，除非收到额外指令。

## 1. 背景与现状

当前 S3 与 WebDAV 共用 `src-tauri/src/services/sync_protocol.rs` 中的同步协议。每次上传都会生成以下完整快照：

- `db.sql`：除少量本机运行数据外的整个数据库 SQL 导出；
- `skills.zip`：Skill 单一事实来源目录的完整压缩包；
- `manifest.json`：上述两个 artifact 的哈希、大小、设备和快照信息。

WebDAV 和 S3 的上传、下载实现分别位于：

- `src-tauri/src/services/webdav_sync.rs`；
- `src-tauri/src/services/s3_sync.rs`。

Skill 文件打包位于 `src-tauri/src/services/webdav_sync/archive.rs`，实际来源可能是 `~/.cc-switch/skills` 或 `~/.agents/skills`。即使用户只修改一个很小的配置，现有实现也可能重新遍历、压缩并上传整个 Skill 目录，导致快照构建慢、传输量大、同步耗时不可预测。

当前下载使用完整快照替换数据库和 Skill 目录。现有 `db.sql` 无法在保持其他云端数据不变的情况下独立更新某一类内容，因此本需求必须升级同步协议，不能只在前端隐藏 `skills.zip`。

## 2. 目标

1. 明确列出当前实际参与云同步的用户数据，并区分本机数据和派生数据。
2. 将云同步拆成可以独立选择的 A–K 十一个类别。
3. A–K 默认全部勾选，以保持升级前的完整同步意图；用户可以逐项关闭。
4. 关闭某类别后，同时停止其上传和下载；本机与云端已有数据均保留，关闭开关不代表删除。
5. S3 与 WebDAV 使用同一套本机勾选配置，但每个远端目标分别记录初始化状态。
6. 未选择 Skill 文件时，不遍历、不压缩、不读取、不上传、不下载且不替换 Skill 文件目录。
7. 下载选中的类别时，只替换获准同步的数据；未选类别必须保持字节级或行级不变。
8. 支持查看每类本机和云端大小、数量及状态。
9. 提供显式、安全的远端数据管理，允许删除未启用类别的新版数据，以及单独删除旧版完整快照。
10. 新版能读取旧版完整快照，但只写新版分项协议；不为旧版双写完整快照。

## 3. 非目标

- 不提供单个供应商、单个 MCP、单条提示词或单个 Skill 的选择。
- 不把上传和下载拆成两套开关。
- 不同步各设备的勾选结果。
- 不在关闭某项时自动删除云端数据。
- 不实现定时过期、自动清理或远端回收站。
- 不为兼容旧客户端继续生成 v2 完整快照。
- 不增加端到端加密。现有敏感数据同步边界保持不变，但界面必须明确提示。
- 不同步项目源代码、用户会话正文、请求日志、用量明细、健康检查结果或本机目录设置。

## 4. 同步类别

类别 ID 是协议字段和代码枚举，发布后不得随文案变化。所有类别默认勾选。

| ID | 界面名称 | 当前数据来源/建议归属 | 同步语义与注意事项 |
|---|---|---|---|
| A `providers` | 供应商配置 | `providers`、`provider_endpoints`、`settings.universal_providers`，以及维持供应商空集合语义所需的种子标记 | 包含 API Key、自定义 Header、当前项、故障队列和路由元数据；选中时云端完整替换本机该类别 |
| B `mcp` | MCP 配置 | `mcp_servers` | 包含服务配置及各应用启用状态；配置中可能包含令牌和环境变量 |
| C `prompts` | 提示词 | `prompts` | 包含正文、说明和各应用启用状态 |
| D `skill_repos` | Skill 仓库来源 | `skill_repos`、`default_skill_repos_initialized` 或等价业务标记 | 不含 Skill 文件；必须保留“用户有意删除全部默认来源”的语义，不能因空集合而重新播种 |
| E `skill_metadata` | Skill 安装清单与状态 | `skills` | 包含来源及各应用启用状态；与 F 存在第 7.4 节定义的单向依赖 |
| F `skill_files` | Skill 文件 | `SkillService::get_ssot_dir()` 返回的整个目录 | 包含 `SKILL.md`、脚本、模板、示例和其他资源；使用确定性 ZIP；启用 F 必须启用 E |
| G `profiles` | Profiles 项目配置 | `profiles`、`current_profile_id_*` | Profile 是 provider/MCP/Skill/prompt ID 的组合引用，不是项目源码；跨类别字段按第 7.5 节过滤 |
| H `common_config` | 通用配置片段 | `settings` 中受支持的 `common_config_*` 内容键及保持清空语义所需的键 | 配置片段可能包含用户写入的敏感字段；只允许显式白名单键进入 artifact |
| I `proxy_settings` | 代理与请求设置 | `proxy_config`、`global_proxy_url`、`rectifier_config`、`optimizer_config`、`copilot_optimizer_config` 等明确归属于该功能的键 | 包含代理、请求修正及优化行为；不得带入请求日志 |
| J `diagnostics_settings` | 检测与日志设置 | `stream_check_config`、`log_config` 等明确归属于检测行为的配置键 | 只同步配置，不同步检测结果、流检查日志或应用日志文件 |
| K `model_pricing` | 模型定价覆盖 | `~/.cc-switch/model-pricing.json` 或代码解析出的实际路径 | JSON 文件是唯一同步事实来源；数据库 `model_pricing` 只是派生状态，下载后从文件重建 |

> 修正要求：实现前应生成一份集中式类别注册表，并复核所有现存 `settings` 键。未知键默认归为 `LocalOnly`，不得继续因为导出整张 `settings` 表而被意外同步。

## 5. 明确不参与同步的数据

以下内容保持设备本地，不进入 A–K：

- `settings.json` 中的设备偏好，包括主题、语言、窗口、目录覆盖、Skill 存储位置/链接方式等；
- S3/WebDAV 地址、存储桶、账号、密码、Access Key、Secret Key、remote root、profile、自动同步开关；
- 本需求新增的 A–K 勾选结果和每个远端目标的初始化状态；
- Claude Desktop gateway token 等设备身份或本机网关秘密；
- 单纯用于数据库升级、一次性审计或本机初始化的标记；
- `proxy_request_logs`、`stream_check_logs`、`provider_health`、`proxy_live_backup`、`usage_daily_rollups`、`session_log_sync`、`session_usage_dedup`；
- Skill 卸载备份目录、临时文件、缓存和应用日志；
- 各 CLI/桌面应用的 live 配置文件。它们在下载成功后由现有 projection 服务按选中类别重新生成；
- 项目源代码、会话正文和用户主目录中的其他文件。

业务种子标记不能按名称粗暴排除。`official_providers_seeded` 应随 A 的业务语义处理，`default_skill_repos_initialized` 应随 D 处理；否则同步得到的空集合可能在重启后被默认数据重新填充。

## 6. 用户界面需求

### 6.1 位置与通用性

在“设置 → 高级 → 云同步”的 S3/WebDAV 配置区增加“同步内容”卡片。S3 与 WebDAV 共用同一套复选框，不在两个 Tab 中维护两份选择。

选择配置只保存在当前设备。切换传输方式后勾选结果不变；远端是否已经初始化则按“传输方式 + 服务器身份 + remote root + profile”分别记录。修改上述任一目标字段后，必须重新计算目标指纹并进入待初始化状态。目标指纹使用不含密码、Secret Key 和令牌的规范字段计算哈希，不得把凭据拼入状态键或日志。

### 6.2 选择列表

- 展示 A–K 十一项，默认全部勾选。
- 每项展示名称、一句话内容说明、本机条数或大小、云端大小、最近同步时间和状态。
- F 展示 Skill 文件的未压缩估算大小、文件数，以及远端压缩包大小。
- 统计必须异步计算，不能阻塞设置页。进入页面不得同步遍历大型 Skill 目录；使用后台任务、缓存和加载占位。
- 旧版快照只有 `db.sql` 和 `skills.zip` 汇总大小时，界面明确显示“旧版合并数据，无法按类别统计”，不能伪造分类大小。
- A、B、H、I 的说明下显示“可能包含 API Key、令牌或其他敏感配置”。

### 6.3 Skill 依赖

- 用户开启 F 时自动开启 E，并说明“同步 Skill 文件需要同时同步安装清单”。
- E 开启期间可以关闭 F。
- 用户关闭 E 时自动关闭 F。
- 禁止形成 F 开启、E 关闭的持久化状态；后端也必须验证，不能只依赖前端。

### 6.4 全部关闭

允许关闭 A–K 全部选项。此时：

- 保存服务器连接配置；
- 显示“云同步已暂停：未选择同步内容”；
- 手动上传、下载和自动同步均不执行；
- 不触发错误通知，不更新成功时间，不清理远端数据。

### 6.5 类别状态

至少支持以下状态：

- `disabled`：当前设备未勾选；
- `pending`：已勾选，但尚未为当前远端确定首次方向；
- `ready`：已完成首次上传或下载，可以参加自动同步；
- `syncing`：操作进行中；
- `synced`：本机与最近确认的远端版本一致；
- `local_changed`：本机已变化，等待上传；
- `remote_changed`：远端版本与本机记录不同；
- `remote_missing`：远端尚无该类别；
- `error`：该类别最近一次操作失败；
- `cleanup_incomplete`：manifest 已不引用数据，但残留 artifact 删除失败。

状态文案需要加入现有中、英、日、德文翻译资源，不允许把协议 ID 直接显示给普通用户。

### 6.6 首次方向

新勾选或重新勾选一个类别后，如果当前目标的远端或本机可能已有数据，该类别进入 `pending`。用户必须选择一次：

- “上传本机数据”：以本机类别完整替换远端类别；
- “下载云端数据”：以远端类别完整替换本机类别。

完成前，该类别不参加自动同步。若远端不存在该类别，禁用“下载云端数据”；若选择上传空集合，仍要上传明确的空 artifact，使“远端为空”与“远端从未同步”可区分。

勾选动作本身不发起网络请求，也不隐式选择方向。

### 6.7 上传与下载确认

沿用现有手动上传/下载确认对话框，并增加：

- 本次参与的类别及大小；
- 因 `disabled` 或 `pending` 被跳过的类别；
- 下载时会被完整替换的本机类别；
- 敏感数据提示；
- 旧版来源提示；
- 预计总传输量。

一次操作中的多个类别必须整体成功或整体回滚。不能把“供应商成功、Skill 失败”显示为整体成功。

## 7. 功能语义

### 7.1 对称关闭

关闭某类别后，在普通上传、下载和自动同步路径中，该设备不会：

- 导出或打包该类别；
- 对该类别执行 HEAD、GET、PUT 或 DELETE；远端数据管理中的用户显式删除是唯一例外；
- 因该类别的数据变化触发自动上传；
- 在下载后修改该类别的数据库行、文件或 live projection。

关闭只影响当前设备的参与范围。云端条目和本机数据继续保留。

### 7.2 选中类别的替换规则

除 E 在 F 关闭时的特殊规则外，下载采用以下语义：

- 远端类别完整替换本机同类别；
- 远端明确的空 artifact 会清空本机同类别；
- 远端缺少类别条目表示“从未同步或已被删除”，不得当作空集合清除本机；
- 未选类别保持操作前状态；
- 删除、排序、当前项和启用状态都属于对应类别并可传播。

### 7.3 自动同步

现有数据库变更通知必须通过集中式类别注册表映射到 A–K：

- 只有映射到已选且 `ready` 类别的变更才触发上传；
- 未选或 `pending` 类别的变化不触发网络操作；
- 一次防抖窗口内合并多个类别，只导出变化且已获准的类别；
- F 关闭时，E 或其他类别的变化不能调用 Skill 目录解析或 ZIP 逻辑；
- K 的文件写入或对应服务修改需要产生 K 类别变化信号；
- 全部关闭时 worker 保持安静。

自动同步继续保持“本机变化上传到云端”的既有方向，不自动下载远端变化。

### 7.4 Skill 清单与文件

F 开启时：

- E 和 F 作为同一原子恢复组；
- 远端 Skill 清单和 SSOT 文件目录共同替换本机对应内容；
- 数据库失败时恢复 Skill 目录备份，文件失败时不得提交数据库变化。

仅 E 开启、F 关闭时：

- 不创建远端存在但本机没有文件的 Skill；
- 不删除本机 Skill 文件或仅存在于本机的 Skill；
- 只对本机已经存在的相同 Skill ID 更新仓库来源字段及各应用启用状态；
- 本机 `directory`、`content_hash`、文件派生名称/说明和安装时间保持不变；
- 返回跳过的远端 Skill 数量，并在详情中说明“缺少本机文件，未导入”。

本节规则优先于第 7.2 节的一般替换规则。

### 7.5 Profiles

G 控制 Profile 实体的 ID、名称、排序、创建/更新时间以及各 scope 的当前 Profile 状态。Profile payload 中的引用槽位按类别权限处理：

- provider 引用只有 A 同时选中时才更新；
- MCP 引用只有 B 同时选中时才更新；
- prompt 引用只有 C 同时选中时才更新；
- Skill 引用只有 E 同时选中时才更新；
- 未选类别对应的本机槽位保持原值；
- 新导入的 Profile 对无权同步的槽位写为未设置，不复制悬空 ID；
- 已获准的引用对象仍不存在时跳过该引用、保留 Profile，并返回结构化警告。

### 7.6 模型定价

- K 的上传 artifact 来自 `model-pricing.json`，不是 `model_pricing` 表；
- 下载 K 时先校验文件结构和支持的 schema version，再原子替换本机 JSON；
- 替换成功后调用现有 `sync_local_model_pricing` 或等价能力重建数据库派生状态；
- K 未选时，同时保留本机 JSON 和数据库中的有效定价状态；
- 旧版 v2 快照没有可靠的 K artifact，读取旧快照时保持本机 K 不变并给出说明。

### 7.7 下载后的 live projection

现有 `run_post_import_sync` 会刷新供应商、提示词、模型定价、设置缓存和用量缓存。新实现需要按实际恢复的类别调用相应 projection，不能因为下载 A 就改写未选择的 C、E 或 K。

需要提供类别到 projection 的显式映射，并把非关键 projection 失败作为结构化 warning 返回；数据库/文件快照已经成功应用时不得谎报为完全失败，但 UI 必须显示“数据已恢复，部分运行配置刷新失败”。

## 8. 同步协议 v3

### 8.1 远端目录

保留按协议、数据库兼容版本和 profile 隔离的结构：

```text
{remoteRoot}/v3/db-v{DB_COMPAT_VERSION}/{profile}/
  manifest.json
  artifacts/
    providers/{sha256}.json
    mcp/{sha256}.json
    prompts/{sha256}.json
    skill_repos/{sha256}.json
    skill_metadata/{sha256}.json
    skill_files/{sha256}.zip
    profiles/{sha256}.json
    common_config/{sha256}.json
    proxy_settings/{sha256}.json
    diagnostics_settings/{sha256}.json
    model_pricing/{sha256}.json
```

artifact 使用内容哈希命名。上传先写不可变 artifact，最后提交 manifest。manifest 是远端可见快照的唯一提交点。

### 8.2 Manifest 最低字段

```json
{
  "format": "cc-switch-cloud-sync",
  "protocolVersion": 3,
  "dbCompatVersion": 6,
  "profile": "default",
  "snapshotId": "sha256-of-category-entries",
  "baseSnapshotId": "previous-snapshot-id-or-null",
  "deviceName": "MacBook",
  "createdAt": "2026-09-05T00:00:00Z",
  "categories": {
    "providers": {
      "schemaVersion": 1,
      "artifact": "artifacts/providers/<sha256>.json",
      "sha256": "<sha256>",
      "size": 1234,
      "itemCount": 8,
      "updatedAt": "2026-09-05T00:00:00Z",
      "deviceName": "MacBook"
    }
  }
}
```

要求：

- `categories` 可以包含当前设备未选择的类别，以保留其他设备或以前上传的数据；
- 上传未选类别时必须复制远端 manifest 中的原条目，不能重新导出或改写；
- 明确的空集合也有 category entry 和合法 artifact；
- 被手动远端删除的类别从 manifest 中移除；
- `snapshotId` 必须覆盖所有 category entry 的 ID、schemaVersion、hash 和路径；
- 限制 manifest 和每个 artifact 的最大尺寸、文件数及解压后总尺寸；
- 校验 ID、相对路径和哈希，拒绝路径穿越、重复条目和未知必需字段；
- 未识别的未来类别保留其 manifest entry，但当前客户端不下载、不删除、不重写。

### 8.3 类别 artifact

数据库类别使用带 `schemaVersion` 的规范 JSON，而不是把整张数据库继续塞进一个 SQL 文件。每个类别实现统一 adapter：

```rust
trait SyncCategoryAdapter {
    fn category(&self) -> SyncCategory;
    fn export(&self, db: &Database) -> Result<CategoryArtifact, AppError>;
    fn validate(&self, artifact: &CategoryArtifact) -> Result<(), AppError>;
    fn replace_in_transaction(
        &self,
        tx: &Transaction,
        artifact: &CategoryArtifact,
        context: &ApplyContext,
    ) -> Result<CategoryApplyReport, AppError>;
}
```

实现不必逐字采用该签名，但必须具备同等的集中注册、导出、校验、替换、统计和变更映射能力。禁止在 S3 与 WebDAV 中复制两套类别逻辑。

JSON 只输出业务白名单字段。`settings` 必须按 key 注册表分类，未知 key 留在本机。导入先进入内存结构或临时数据库验证，不能直接执行远端任意 SQL。

### 8.4 上传流程

1. 获取跨 S3/WebDAV 共用的全局同步锁。
2. 读取本机选择和当前远端目标状态；全部关闭时直接返回 paused。
3. 获取远端 v3 manifest、ETag 和 snapshot ID；不存在时建立空基线。
4. 只导出已选、`ready` 且需要上传的类别。手动“上传本机数据”可初始化 `pending` 类别。
5. 未选或不参与本次上传的类别沿用远端 manifest entry。
6. 对 artifact 计算哈希；哈希已存在时跳过 PUT。
7. 上传所有新增 artifact 并逐一校验结果。
8. 使用 `If-Match`/等价条件写入新 manifest；远端已变化时返回冲突，不覆盖其他设备的新快照。
9. manifest 成功后更新本机逐类状态。
10. manifest 提交失败时，已上传但未被引用的 artifact 记为待清理残留；本期不在普通同步中自动删除，由“管理云端数据”展示并让用户显式清理。

### 8.5 下载流程与本机原子性

1. 获取全局同步锁并暂停自动上传信号。
2. 获取 manifest，并确定本次已选且允许下载的类别。
3. 下载所有需要的 artifact 到临时目录，完成格式、版本、大小、文件数和 SHA-256 校验。
4. 在修改本机前完成全部类别预校验。
5. 备份 SQLite、本次涉及的 Skill SSOT 和 `model-pricing.json`。
6. 在一个 SQLite 事务中应用所有数据库类别；文件先写临时路径，再以原子 rename 切换。
7. 任一类别失败时回滚数据库并恢复所有文件备份。
8. 全部成功后提交数据库事务，清理备份并按类别执行 live projection。
9. 返回逐类结果、跳过项和 projection warnings。

实现必须通过失败注入测试证明：数据库失败、ZIP 解压失败、模型定价文件写入失败和 projection 失败不会产生未声明的半恢复状态。

### 8.6 远端并发

本机 mutex 只能防止同一进程内的并发，不能替代多设备并发控制。manifest 的更新和远端清理必须使用 ETag、版本 ID 或同等条件提交：

- 条件不满足时返回 `remote_changed`；
- UI 刷新远端状态并要求用户重新确认；
- 对不支持安全条件写的 S3 兼容服务，禁用远端删除并明确提示服务能力不足；
- 不得用“先 HEAD 再无条件 PUT”的方式宣称并发安全。

## 9. 旧协议兼容与升级

### 9.1 读取 v2

新版优先读取 v3。v3 不存在时允许读取现有 v2 `manifest.json + db.sql + skills.zip`：

- 把 `db.sql` 导入隔离的临时 SQLite 数据库；
- 使用 v3 的类别 adapter 从临时数据库提取用户选中的 A–J；
- 只有 F 选中时才下载和恢复旧版 `skills.zip`；
- K 在 v2 中视为不存在，保留本机；
- 未选类别不得从旧 SQL 应用到本机；
- v2 下载成功不改写或删除 v2 远端文件。

严禁通过字符串拆分 SQL 来提取表。

### 9.2 写入 v3

- 新版只写 v3，不更新 v2；
- 旧客户端继续看到最后一次 v2 快照，不能看到新版后续变化；
- 界面提示“选择性同步要求所有参与设备升级”；
- 不提供 v2 双写模式，因为它会继续生成完整 `skills.zip` 并抵消性能优化。

### 9.3 本机设置迁移

- 新字段缺失时 A–K 全部为选中；
- 旧配置升级后，如果只有 v2 远端，选中类别显示“需要迁移”，暂停这些类别的自动同步，要求用户选择“上传本机数据”或“下载旧版云端数据”；
- 已存在有效 v3 状态时按目标指纹恢复逐类状态；
- 反序列化遇到未知类别时保留原值，当前 UI 不启用它；
- 不需要仅为此需求增加 SQLite schema；如实现最终确需 migration，必须说明原因并验证重复迁移。

## 10. 远端数据管理

### 10.1 入口与列表

在同步内容卡片提供“管理云端数据”。打开后重新读取远端 manifest，不使用可能过期的设置页缓存。按协议版本和 profile 显示：

- 类别、大小、条数或文件数；
- 最近上传时间、设备、hash 简写；
- 当前设备是否启用；
- 是否可删除；
- 未被任何当前 manifest 引用的已知残留 artifact；
- v2 完整快照的 `db.sql`、`skills.zip` 和合计大小。

### 10.2 删除新版类别

- 只能选择当前设备已关闭的 v3 类别；
- 删除前显示类别、总大小、目标服务器、remote root 和 profile；
- 二次确认明确说明：其他设备仍启用时可能再次上传；
- 删除操作获取全局同步锁并暂停当前目标自动同步；
- 提交前重新获取 manifest，并以 ETag/snapshot ID 作为条件；
- 先条件提交一份移除目标 category entry 的新 manifest，再删除已无引用的 artifact；
- DELETE 的 404 视为幂等成功；
- manifest 提交失败时不得删除 artifact；
- manifest 成功而 artifact DELETE 失败时标记 `cleanup_incomplete`，正常下载不再看到该类别，并允许重试残留清理；
- 一个批次删除多个类别时，manifest 只提交一次。

S3 需要补充签名正确的 `DeleteObject`，WebDAV 需要补充 `DELETE`。两个传输都应复用协议层的清理编排。

### 10.3 删除旧版完整快照

旧版不能按类别删除，必须提供独立操作“删除旧版完整快照”：

- 只有当前目标已有完整、校验通过的 v3 manifest 时才能启用；
- 确认框列出旧版路径、`db.sql`、`skills.zip`、manifest 和合计大小；
- 明确提示删除后旧客户端无法从该 profile 继续恢复；
- 先删除旧版 manifest，使旧客户端停止发现该快照，再删除 `db.sql` 和 `skills.zip`；
- 同时识别现有 current 与 legacy WebDAV 布局，按具体路径分别展示和确认；
- 某个文件删除失败时记录可重试状态，不把另一个布局或 profile 一并删除。

## 11. 错误、安全与隐私

- A、B、H、I 可能含明文密钥、令牌、Header 或环境变量。本期不尝试通过字段名猜测并过滤秘密，避免产生“已过滤”的错误安全承诺。
- 上传确认和首次启用说明必须告知用户远端会保存敏感配置。
- 沿用凭据掩码展示；日志、错误、manifest 和统计信息不得写入秘密值或配置正文。
- manifest 只记录类别元数据，不记录业务内容摘要以外的明文数据。
- WebDAV URL 和 S3 endpoint 的既有安全校验不得弱化。
- ZIP 继续执行路径穿越、符号链接/目录循环、条目数、单项大小和总解压大小保护。
- 类别 JSON 必须限制总大小、数组长度、字符串长度和 schema version。
- 下载前的确认必须使用刚刚读取的远端 snapshot ID；确认期间远端变化时中止。
- 所有删除命令只接受协议层生成并验证过的精确 key/URL，不接受用户直接传入任意删除路径。

## 12. 建议代码结构与修改范围

### 12.1 Rust

重点修改或新增：

- `src-tauri/src/services/sync_protocol.rs`
  - `SyncCategory`、v3 manifest、类别注册表、上传/下载/清理编排；
- `src-tauri/src/services/sync_categories/`
  - A–K adapters、settings key 白名单、统计和 projection 映射；
- `src-tauri/src/services/s3_sync.rs`、`webdav_sync.rs`
  - 仅保留传输适配，接入 v3 协议；
- `src-tauri/src/services/s3.rs`、`webdav.rs`
  - 条件提交、DELETE、能力错误；
- `src-tauri/src/services/s3_auto_sync.rs`、`webdav_auto_sync.rs`
  - 按类别过滤变化和 pending 状态；
- `src-tauri/src/commands/s3_sync.rs`、`webdav_sync.rs`
  - 范围查询、初始化方向、统计、远端清理命令；
- `src-tauri/src/commands/sync_support.rs`
  - 按类别执行 post-import projection；
- `src-tauri/src/settings.rs`
  - 设备本地的共享选择和按目标指纹保存的逐类状态；
- `src-tauri/src/database/backup.rs`
  - 保留 v2 隔离导入，新增按类别原子替换所需能力；不得继续把 v3 实现为整库导入。

### 12.2 前端

重点修改：

- `src/components/settings/WebdavSyncSection.tsx`
  - 同步内容卡片、逐类状态、首次方向、确认详情和远端管理；
- `src/types.ts`
  - `SyncCategory`、选择、统计、逐类状态、v3 remote info 和 cleanup report；
- `src/lib/api/settings.ts`
  - 新 Tauri commands 的类型安全封装；
- 现有 i18n locale 文件
  - 中、英、日、德文案；
- `tests/components/WebdavSyncSection.test.tsx`
  - 默认值、依赖、暂停、pending、确认和删除交互。

如果 `WebdavSyncSection.tsx` 因新增能力继续膨胀，应拆为传输配置、同步内容、状态列表和远端数据管理组件；不要在同一文件复制 S3/WebDAV 的业务分支。

## 13. 接口返回要求

命令返回不得只给 `success: true`。至少返回：

```ts
interface SyncOperationReport {
  snapshotId?: string;
  sourceProtocolVersion?: number;
  status: "success" | "paused" | "conflict" | "error";
  categories: Array<{
    category: SyncCategory;
    action: "uploaded" | "downloaded" | "unchanged" | "skipped";
    bytes: number;
    itemCount?: number;
    warningCode?: string;
  }>;
  warnings: Array<{ code: string; category?: SyncCategory; message: string }>;
}
```

远端信息返回每类 artifact 元数据、v2/v3 布局、是否兼容及可删除能力。前端按稳定 code 翻译错误，不能依赖解析后端英文文本。

## 14. 用户故事与验收标准

### 故事 1：关闭 Skill 文件同步

作为拥有大量 Skills 的用户，我希望保留其他配置的云同步，同时停止传输 Skill 文件。

- [ ] A–K 初次显示全部勾选。
- [ ] 关闭 F 后，E 可以保持开启。
- [ ] 上传供应商或提示词变化时，不解析 Skill SSOT、不创建 ZIP，也不请求远端 Skill artifact。
- [ ] 下载时本机 Skill 文件目录保持不变。
- [ ] UI 显示本次未传输 F 以及节省的远端 artifact 大小。

### 故事 2：选择性恢复

作为在不同设备使用不同配置的用户，我希望只下载指定类别。

- [ ] 选择 A、B 下载时，只有供应商和 MCP 被远端完整替换。
- [ ] C–K 的数据库行、文件和 live 配置保持不变。
- [ ] 远端明确空类别能清空本机对应类别；远端缺失类别不会清空本机。
- [ ] 任一选中类别校验失败时 A、B 均保持下载前状态。

### 故事 3：只同步 Skill 状态

- [ ] E 开、F 关时，只更新本机已有相同 ID Skill 的仓库来源和应用启用状态。
- [ ] 远端独有 Skill 不在本机生成空安装记录。
- [ ] 本机 Skill 的目录、内容 hash、文件和安装时间保持不变。
- [ ] 返回缺少文件而跳过的数量和可理解说明。

### 故事 4：首次方向与自动同步

- [ ] 新开启的类别进入 pending，用户未选方向前不会被自动上传或下载。
- [ ] 选择上传后，以本机初始化远端并进入 ready。
- [ ] 选择下载后，以远端初始化本机并进入 ready。
- [ ] 未选类别的数据库变化不会触发任何云端请求。
- [ ] 全部关闭后保存配置但不发生同步。

### 故事 5：Profiles 独立性

- [ ] G 开、A 关时，下载 Profile 不修改本机 provider 引用槽位。
- [ ] G 与 B 同时开启时，MCP 槽位按远端更新。
- [ ] 缺少引用对象时跳过该引用并返回 warning，页面和数据库不产生不可用强引用。

### 故事 6：模型定价

- [ ] K 上传的内容来自真实 `model-pricing.json`。
- [ ] 下载后 JSON 与数据库派生状态一致。
- [ ] K 关闭时下载其他类别不会修改 JSON 或有效定价数据。
- [ ] v2 下载不会用旧数据库表覆盖本机 K。

### 故事 7：远端清理

- [ ] 只能选择本机已关闭的 v3 类别删除。
- [ ] manifest 条件提交冲突时没有 artifact 被删除。
- [ ] manifest 更新成功、artifact 删除失败时正常同步不再引用目标数据，并显示可重试残留。
- [ ] 404 删除按成功处理。
- [ ] 仍开启该类别的其他设备可以再次上传，确认框已明确提示。
- [ ] 旧版快照只能通过独立操作删除，且需要有效 v3 快照。

### 故事 8：旧版迁移

- [ ] 新版可以从 v2 SQL 临时库提取选中的数据库类别。
- [ ] F 关闭时不下载 v2 `skills.zip`。
- [ ] 新版上传只写 v3。
- [ ] 旧版客户端继续看到最后 v2 快照，不会误读 v3。

## 15. 测试要求

### 15.1 Rust 单元与数据库集成测试

必须覆盖：

- A–K 默认全选及未知类别兼容；
- E/F 依赖的前后端校验；
- settings key 注册表：所有已知业务键被明确分类，未知键默认 LocalOnly；
- 每个 adapter 的确定性导出、空集合和完整替换；
- 未选类别在选择性恢复前后保持一致；
- E-only 交集更新规则；
- Profile payload 按选择过滤；
- K 文件与数据库重建；
- manifest hash、路径、版本、大小及 SHA 校验；
- v2 临时数据库提取；
- 多类别失败注入和文件/数据库回滚；
- 自动同步变更到类别的映射；
- F 关闭时 ZIP 函数未被调用；
- 条件提交冲突和清理幂等性；
- manifest 先更新、artifact 后删除的失败组合。

### 15.2 前端测试

使用现有 Vitest、Testing Library 和 MSW/Tauri mock 覆盖：

- 默认 A–K 全选；
- F 开启强制 E、关闭 E 联动关闭 F；
- 全关闭显示暂停且操作按钮不可执行；
- 每类加载、ready、pending、error 和 cleanup 状态；
- pending 方向选择；
- 上传/下载确认清单及敏感数据提示；
- 远端管理只允许删除未选类别；
- v2 删除使用单独确认；
- 冲突后刷新状态，不显示成功 toast。

### 15.3 协议级真实验收

至少使用一个真实运行的 S3 兼容服务和一个真实运行的 WebDAV 服务进行协议验收。可以使用本地 MinIO 和独立 WebDAV 服务，但必须走真实 HTTP、签名、ETag/条件请求、PUT、GET、HEAD 和 DELETE，不能用内存 mock 替代。

使用两个相互隔离的 CC Switch 数据目录模拟设备 A/B，完成：

1. A 全量上传 v3，B 只下载 A/B/C/K；
2. A 关闭 F 后修改 provider，证明没有 Skill ZIP 和相关请求；
3. B 保留自己的 Skill 目录和未选类别；
4. 两设备并发修改 manifest，证明旧 ETag 提交失败；
5. 删除未选 v3 类别，验证 commit point 和失败重试；
6. 建立 v2 快照，再由 v3 客户端选择性下载；
7. 校验 v3 后删除 v2 manifest 和 artifacts；
8. 检查日志中没有密钥或配置正文。

如果环境没有真实 AWS S3、R2、OSS、COS 等外部服务凭据，交付时只能声明“S3 兼容服务验收通过”，不得扩展为所有云厂商已验证。

### 15.4 建议验证命令

根据仓库实际脚本执行，至少包括：

```bash
pnpm test:unit
pnpm typecheck
pnpm build:renderer
cd src-tauri && cargo test
```

若命令因环境或项目脚本变化而调整，在交付说明中列出实际命令、退出码和未执行项。

## 16. 成功指标

- F 关闭时，Skill 文件相关的本机读取、压缩和网络请求数为 0。
- 任一关闭类别的导出、上传、下载、恢复和自动同步触发数为 0。
- UI 展示的预计传输量与 manifest 中本次实际 artifact 大小一致。
- 选择性下载不会修改未选类别；由自动化快照对比证明。
- 多类别失败恢复后不存在部分提交；由失败注入测试证明。
- 默认全选时，除本文明确修正的模型定价和设备本地数据边界外，用户可见同步结果与当前完整同步等价。
- 同步内容统计和 Skill 目录扫描不阻塞渲染线程；设置页面可以立即交互。

不设置固定“多少秒完成”的网络耗时目标，因为实际速度取决于远端服务和带宽。性能验收以不处理未选数据、实际传输字节和 UI 可响应性为准。

## 17. 实施顺序

1. 建立 A–K 类别注册表、settings key 白名单和测试。
2. 实现 v3 manifest、类别 artifact adapters 和本机原子恢复。
3. 将 S3/WebDAV 接到共享 v3 编排，并实现条件提交。
4. 改造自动同步，只响应选中且 ready 的类别。
5. 实现本机选择、按目标初始化状态、统计 API 和前端选择区。
6. 实现 v2 选择性读取和升级提示。
7. 实现远端管理、S3/WebDAV DELETE 和旧版快照清理。
8. 完成 i18n、用户文档、单元/集成测试和真实协议验收。

不要先实现远端删除再补 manifest 并发保护；删除必须建立在 v3 commit point 和条件更新已经可靠的基础上。

## 18. 完成定义

只有同时满足以下条件才算完成：

- A–K 所有选项真实控制上传和下载；
- 默认全选、设备本地保存、S3/WebDAV 共用选择；
- Skill、Profile、模型定价和全关闭的特殊语义全部实现；
- v3 可写、v2 可选择性读取、旧客户端边界有明确提示；
- 多类别恢复可回滚，远端删除具有条件提交和安全失败状态；
- 前端状态、大小、确认、错误和 i18n 完整；
- Rust、前端和协议测试通过；
- 真实 S3 兼容服务及 WebDAV 验收有可复现证据；
- README 或用户手册已更新，说明同步内容、敏感信息、升级要求和远端清理行为；
- 交付说明列出修改文件、验证证据、仍未验证的外部厂商及任何偏离本文的实现决定。
