# 09 - 通道集成

## 概述

将 AI 聊天能力接入第三方即时通讯平台（Telegram、飞书、钉钉、微信），实现统一消息协议、插件化平台接入、用户配对授权、per-chat 会话隔离和流式消息更新。

**源码位置**：`process/channels/`、`process/bridge/channelBridge.ts`、`process/bridge/weixinLoginBridge.ts`、`process/webserver/routes/weixinLoginRoutes.ts`

> **设计决策**：原实现采用单例 + 全局事件总线的模式，模块间耦合较深（`ChannelManager` → `PluginManager` → `ActionExecutor` → `ChannelMessageService` 形成长调用链）。Rust 重写时建议通过 trait 解耦各层，并将"平台适配"与"消息路由"分离为独立子模块。

## 子模块划分

| 子模块 | 原始源码 | Rust 归属建议 |
|--------|---------|--------------|
| 插件管理与生命周期 | `core/ChannelManager.ts`、`gateway/PluginManager.ts` | `aionui-channel` |
| 会话隔离 | `core/SessionManager.ts` | `aionui-channel` |
| 消息路由与 Action | `gateway/ActionExecutor.ts`、`actions/` | `aionui-channel` |
| 消息发送与流式回调 | `agent/ChannelMessageService.ts`、`agent/ChannelEventBus.ts` | `aionui-channel` |
| 配对授权 | `pairing/PairingService.ts` | `aionui-channel` |
| 平台插件（Telegram） | `plugins/telegram/` | `aionui-channel`（feature flag） |
| 平台插件（飞书） | `plugins/lark/` | `aionui-channel`（feature flag） |
| 平台插件（钉钉） | `plugins/dingtalk/` | `aionui-channel`（feature flag） |
| 平台插件（微信） | `plugins/weixin/` | `aionui-channel`（feature flag） |
| 凭据加解密 | `utils/credentialCrypto.ts` | `aionui-common` |
| IPC 桥接 | `bridge/channelBridge.ts`、`bridge/weixinLoginBridge.ts` | `aionui-channel`（HTTP 路由） |
| 微信登录路由 | `webserver/routes/weixinLoginRoutes.ts` | `aionui-channel`（HTTP 路由） |

---

## IPC 接口

### 插件管理

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `channel.get-plugin-status` | HTTP | 无 | `IChannelPluginStatus[]` | 获取所有已注册插件的状态（含扩展插件元数据） |
| `channel.enable-plugin` | HTTP | `{ pluginId: string, config: Record<string, unknown> }` | `IBridgeResponse` | 启用插件，保存配置后启动连接 |
| `channel.disable-plugin` | HTTP | `{ pluginId: string }` | `IBridgeResponse` | 停用插件，停止连接并更新状态 |
| `channel.test-plugin` | HTTP | `{ pluginId: string, token: string, extraConfig?: { appId?, appSecret? } }` | `{ success: boolean, botUsername?: string, error?: string }` | 测试插件凭据是否有效，返回 bot 用户名 |

### 配对管理

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `channel.get-pending-pairings` | HTTP | 无 | `IChannelPairingRequest[]` | 获取所有待审批的配对请求 |
| `channel.approve-pairing` | HTTP | `{ code: string }` | `IBridgeResponse` | 批准配对请求，创建授权用户记录 |
| `channel.reject-pairing` | HTTP | `{ code: string }` | `IBridgeResponse` | 拒绝配对请求 |

### 用户管理

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `channel.get-authorized-users` | HTTP | 无 | `IChannelUser[]` | 获取所有已授权用户列表 |
| `channel.revoke-user` | HTTP | `{ userId: string }` | `IBridgeResponse` | 撤销用户授权，清除其会话 |

### 会话管理

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `channel.get-active-sessions` | HTTP | 无 | `IChannelSession[]` | 获取所有活跃的通道会话 |

### 设置同步

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `channel.sync-channel-settings` | HTTP | `{ platform, agent: { backend, customAgentId?, name? }, model?: { id, useModel } }` | `IBridgeResponse` | 同步指定平台的 Agent 和模型配置到运行时 |

### 事件推送

| 通道名 | 方向 | 载荷 | 功能语义 |
|--------|------|------|---------|
| `channel.pairing-requested` | 服务端 → 客户端 | `IChannelPairingRequest` | IM 用户请求配对时推送给前端 |
| `channel.plugin-status-changed` | 服务端 → 客户端 | `{ pluginId: string, status: IChannelPluginStatus }` | 插件状态变更时推送（启动、停止、错误等） |
| `channel.user-authorized` | 服务端 → 客户端 | `IChannelUser` | 配对批准后推送给前端 |

> **协议映射**：原实现中事件推送通过 Electron IPC `emit` 实现。Rust 重写时这三个事件应通过 WebSocket 推送（复用 `07-realtime.md` 的 WebSocket 通道），前端订阅相应事件类型即可。

---

## HTTP 路由

### 微信 QR 码登录（SSE）

```
GET /api/channel/weixin/login
```

**认证**：需要 API 访问验证（`validateApiAccess` 中间件）

**响应类型**：`text/event-stream`（Server-Sent Events）

**SSE 事件序列**：

| 事件名 | 载荷 | 说明 |
|--------|------|------|
| `qr` | `{ qrcodeData: string }` | QR 码原始票据（前端自行渲染为二维码） |
| `scanned` | `{}` | 用户已扫码 |
| `done` | `{ accountId: string, botToken: string, baseUrl: string }` | 登录成功，返回 Bot 凭据 |
| `error` | `{ message: string }` | 登录失败 |

**流程**：
1. 调用微信 iLink Bot API 获取 QR 码
2. 轮询扫码状态（`wait` → `scanned` → `confirmed`）
3. 扫码确认后返回 Bot Token
4. 前端使用返回的凭据调用 `channel.enable-plugin` 启用微信插件

---

## 插件系统

### 插件抽象接口

```
BasePlugin
├── type: PluginType                              // 平台标识
├── status: PluginStatus                          // 生命周期状态
├── error: string | null                          // 最近一次错误
│
├── initialize(config) → void                     // 初始化（加载配置）
├── start() → void                                // 启动连接
├── stop() → void                                 // 停止连接
│
├── sendMessage(chatId, message) → messageId       // 发送消息
├── editMessage(chatId, messageId, message) → void // 编辑已发消息
├── getActiveUserCount() → number                  // 活跃用户数
├── getBotInfo() → BotInfo | null                  // Bot 信息
│
├── onMessage(handler) → void                      // 注册消息回调
└── onConfirm(handler) → void                      // 注册确认回调
```

**生命周期状态机**：

```
created → initializing → ready → starting → running → stopping → stopped
              ↓                    ↓           ↓
            error ←←←←←←←←←←←←←←←←←←←←←←←←←←←
```

### 平台实现

| 平台 | 连接方式 | 消息更新策略 | 消息长度上限 | SDK / 协议 |
|------|---------|-------------|-------------|------------|
| Telegram | 长轮询 | 编辑消息 | 4096 字符 | grammY |
| 飞书（Lark） | WebSocket | 互动卡片更新 | 4000 字符 | `@larksuiteoapi/node-sdk` |
| 钉钉（DingTalk） | WebSocket Stream | AI Card 流式更新 | 4000 字符 | `dingtalk-stream` |
| 微信（WeChat） | 长轮询 | 新消息回复 | 无特殊限制 | iLink Bot HTTP API |

> **设计决策**：各平台 SDK 差异较大，Rust 重写时建议每个平台作为独立 feature flag 编译，避免不需要的平台依赖。核心框架仅依赖 `ChannelPlugin` trait。

#### Telegram 特性

- 重连机制：指数退避，最多 10 次，最大延迟 30s
- UI 元素：Reply Keyboard（底部按钮）+ Inline Keyboard（消息内联按钮）
- 回调解析：从按钮 callback_data 中提取 `category` 和 `action`

#### 飞书特性

- 事件去重：`processedEvents: Map<eventId, timestamp>`，TTL 5 分钟
- 消息格式：所有响应以互动卡片发送（飞书仅支持编辑卡片，不支持编辑普通消息）
- 事件类型：`im.message.receive_v1`（消息）、`card.action.trigger`（卡片按钮，3 秒内响应）、`application.bot.menu_v6`（自定义菜单）

#### 钉钉特性

- AI Card 流式更新流程：
  1. `POST /v1.0/card/instances` — 创建卡片实例
  2. `POST /v1.0/card/instances/deliver` — 投递到用户/群
  3. `PUT /v1.0/card/streaming` — 流式写入内容
  4. `PUT /v1.0/card/instances` — 标记完成
- 降级策略：AI Card → sessionWebhook → Open API
- chatId 编码：私聊 `user:{staffId}`，群聊 `group:{conversationId}`

#### 微信特性

- 登录方式：QR 码扫码登录（iLink Bot API）
- 消息监听：长轮询 `POST /ilink/bot/getupdates`
- 文件处理：AES-128-ECB 加解密，最大 200MB
- 消息类型：文本、语音、图片、文件、卡片
- 响应超时：5 分钟
- 重试策略：最多 3 次连续失败，30s 退避

---

## 统一消息协议

### 入站消息（Platform → System）

```
IUnifiedIncomingMessage {
  id: string                              // 消息 ID
  platform: PluginType                     // 来源平台
  chatId: string                           // 聊天 ID（群/私聊）
  user: IUnifiedUser                       // 发送者
  content: IUnifiedMessageContent          // 消息内容
  timestamp: number                        // 时间戳
  replyToMessageId?: string                // 回复目标
  action?: IMessageAction                  // 按钮回调动作
  raw?: unknown                            // 平台原始数据
}

IUnifiedUser {
  id: string
  username?: string
  displayName: string
  avatarUrl?: string
}

IUnifiedMessageContent {
  type: MessageContentType                 // 'text' | 'photo' | 'document' | 'voice' | 'audio' | 'video' | 'sticker' | 'action' | 'command'
  text: string
  attachments?: IUnifiedAttachment[]
}
```

### 出站消息（System → Platform）

```
IUnifiedOutgoingMessage {
  type: 'text' | 'image' | 'file' | 'buttons'
  text?: string
  parseMode?: 'HTML' | 'MarkdownV2' | 'Markdown'
  buttons?: IActionButton[][]              // 操作按钮（二维数组：行 × 列）
  keyboard?: IActionButton[][]             // 固定键盘按钮
  imageUrl?: string
  fileUrl?: string
  fileName?: string
  mediaActions?: IChannelMediaAction[]     // 附件操作
  replyToMessageId?: string
  silent?: boolean                         // 静默发送（无通知）
}

IActionButton {
  label: string                            // 按钮文本
  action: string                           // 动作标识
  params?: Record<string, string>          // 动作参数
}
```

---

## Action 系统

### Action 分类

| 分类 | 动作 | 功能 |
|------|------|------|
| **platform** | `pairing.show` | 生成并显示配对码 |
| | `pairing.refresh` | 刷新配对码 |
| | `pairing.check` | 检查配对状态 |
| | `pairing.help` | 配对帮助 |
| **system** | `session.new` | 创建新会话 |
| | `session.status` | 显示会话状态 |
| | `help.show` | 显示帮助 |
| | `help.features` | 显示功能列表 |
| | `help.pairing` | 配对帮助 |
| | `help.tips` | 使用技巧 |
| | `settings.show` | 设置引导 |
| | `agent.show` | 显示 Agent 列表 |
| | `agent.select` | 切换 Agent（参数：`agentType`） |
| **chat** | `chat.send` | 发送消息 |
| | `chat.regenerate` | 重新生成回复 |
| | `chat.continue` | 继续生成 |
| | `action.copy` | 复制提示 |
| | `system.confirm` | 工具确认（参数：`callId`、`value`） |

### Action 上下文

```
IUnifiedAction {
  action: string                           // 动作标识（如 "session.new"）
  category: 'platform' | 'system' | 'chat'
  params?: Record<string, string>
  context: {
    platform: PluginType
    userId: string
    chatId: string
    messageId?: string
    sessionId?: string
  }
}
```

### Action 响应

```
IActionResponse {
  text?: string
  parseMode?: 'HTML' | 'MarkdownV2' | 'Markdown'
  buttons?: IActionButton[][]
  keyboard?: IActionButton[][]
  behavior: 'send' | 'edit' | 'answer'    // 响应行为
  toast?: string                           // 浮动提示
  editMessageId?: string                   // 编辑目标消息
}
```

---

## 核心流程

### 消息处理流程

```
IM 用户发送消息
    ↓
平台插件将消息转为 IUnifiedIncomingMessage
    ↓
ActionExecutor.handleIncomingMessage()
    ↓
授权检查 → isUserAuthorized()
    ├─ 未授权 → 触发配对流程（生成 6 位配对码）
    └─ 已授权 ↓
        ├─ 按钮回调 → 解析 action，路由到对应 Handler
        └─ 文本消息 ↓
            ├─ 查询/创建 Session（per-chat 复合键隔离）
            ├─ 查询/创建 Conversation
            ├─ 发送 "⏳ Thinking..." 占位消息
            ├─ ChannelMessageService.sendMessage() → AI Agent
            │   ↓
            │   流式响应（节流 500ms）→ editMessage() 更新消息
            │   ↓
            │   工具调用 → 等待确认（15s 超时）→ 继续生成
            │
            └─ 流结束 → 添加操作按钮 → editMessage() 最终更新
```

### 配对授权流程

```
1. IM 用户首次发消息 → 授权检查失败
   ↓
2. PairingService 生成 6 位配对码（10 分钟有效）
   ↓
3. 存入 assistant_pairing_codes 表（status='pending'）
   ↓
4. 推送 channel.pairing-requested 事件到前端
   ↓
5. 本地用户在 Settings UI 点击 "Approve"
   ↓
6. PairingService.approvePairing()
   ├─ 创建 assistant_users 记录
   ├─ 更新配对码状态为 'approved'
   └─ 推送 channel.user-authorized 事件
   ↓
7. 用户再次发消息时通过授权检查，进入正常聊天流程
```

### 微信登录流程

```
1. 前端发起 GET /api/channel/weixin/login（SSE）
   ↓
2. 后端调用 iLink Bot API 获取 QR 码
   GET /ilink/bot/get_bot_qrcode?bot_type=3
   ↓
3. SSE 推送 qr 事件（前端渲染二维码）
   ↓
4. 后端轮询扫码状态
   GET /ilink/bot/get_qrcode_status?qrcode=<ticket>
   ↓
5. 状态变化时推送 SSE 事件
   ├─ scanned → 用户已扫码
   ├─ confirmed → SSE 推送 done 事件（含 accountId, botToken）
   └─ expired → SSE 推送 error 事件
   ↓
6. 前端使用返回的凭据调用 channel.enable-plugin 启用微信插件
```

---

## 数据模型

### 插件配置表 `assistant_plugins`

| 列名 | 类型 | 约束 | 说明 |
|------|------|------|------|
| `id` | TEXT | PK | 插件 ID |
| `type` | TEXT | NOT NULL, CHECK | 平台类型：`telegram` / `slack` / `discord` / `lark` / `dingtalk` / `weixin` |
| `name` | TEXT | NOT NULL | 插件显示名称 |
| `enabled` | INTEGER | NOT NULL, DEFAULT 0 | 是否启用 |
| `config` | TEXT | NOT NULL | JSON：`{ credentials: IPluginCredentials, config: IPluginConfigOptions }` |
| `status` | TEXT | | 运行状态 |
| `last_connected` | INTEGER | | 最后连接时间戳 |
| `created_at` | INTEGER | NOT NULL | 创建时间戳 |
| `updated_at` | INTEGER | NOT NULL | 更新时间戳 |

### 授权用户表 `assistant_users`

| 列名 | 类型 | 约束 | 说明 |
|------|------|------|------|
| `id` | TEXT | PK | 用户 ID |
| `platform_user_id` | TEXT | NOT NULL | 平台侧用户 ID |
| `platform_type` | TEXT | NOT NULL | 平台类型 |
| `display_name` | TEXT | | 显示名称 |
| `authorized_at` | INTEGER | NOT NULL | 授权时间戳 |
| `last_active` | INTEGER | | 最后活跃时间 |
| `session_id` | TEXT | | 关联会话 ID |
| | | UNIQUE(platform_user_id, platform_type) | 同一平台用户唯一 |

### 通道会话表 `assistant_sessions`

| 列名 | 类型 | 约束 | 说明 |
|------|------|------|------|
| `id` | TEXT | PK | 会话 ID |
| `user_id` | TEXT | NOT NULL, FK → assistant_users(id) ON DELETE CASCADE | 关联用户 |
| `agent_type` | TEXT | NOT NULL, CHECK | Agent 类型：`gemini` / `acp` / `codex` / `openclaw-gateway` |
| `conversation_id` | TEXT | FK → conversations(id) ON DELETE SET NULL | 关联对话 |
| `workspace` | TEXT | | 工作区路径 |
| `chat_id` | TEXT | | per-chat 隔离键 |
| `created_at` | INTEGER | NOT NULL | 创建时间戳 |
| `last_activity` | INTEGER | NOT NULL | 最后活跃时间 |

### 配对码表 `assistant_pairing_codes`

| 列名 | 类型 | 约束 | 说明 |
|------|------|------|------|
| `code` | TEXT | PK | 6 位配对码 |
| `platform_user_id` | TEXT | NOT NULL | 请求者平台用户 ID |
| `platform_type` | TEXT | NOT NULL | 平台类型 |
| `display_name` | TEXT | | 请求者显示名 |
| `requested_at` | INTEGER | NOT NULL | 请求时间戳 |
| `expires_at` | INTEGER | NOT NULL | 过期时间戳 |
| `status` | TEXT | NOT NULL, DEFAULT 'pending', CHECK | `pending` / `approved` / `rejected` / `expired` |

### conversations 表扩展

> 来自 Migration v12、v14，为 conversations 表增加：
> - `source` 列：`'aionui'` / `'telegram'` / `'lark'` / `'dingtalk'` / `'weixin'`
> - `channel_chat_id` 列：per-chat 隔离键（与 `assistant_sessions.chat_id` 对应）

---

## 插件凭据结构

```
IPluginCredentials {
  // Telegram
  token?: string                           // Bot Token

  // 飞书
  appId?: string
  appSecret?: string
  encryptKey?: string
  verificationToken?: string

  // 钉钉
  clientId?: string
  clientSecret?: string

  // 微信
  accountId?: string                       // iLink Bot 账户 ID
  botToken?: string                        // iLink Bot Token

  [key: string]: unknown                   // 扩展插件字段
}
```

```
IPluginConfigOptions {
  mode?: 'polling' | 'webhook' | 'websocket'   // 连接模式
  webhookUrl?: string                           // Webhook URL
  rateLimit?: number                            // 速率限制
  requireMention?: boolean                      // 群聊是否需要 @Bot
  [key: string]: unknown                        // 扩展配置
}
```

---

## 关键常量

| 常量 | 值 | 说明 |
|------|---|------|
| 配对码长度 | 6 位 | 纯数字 |
| 配对码有效期 | 10 分钟 | 过期自动清理 |
| 过期清理间隔 | 60 秒 | 定时器清理过期配对码 |
| 流式节流间隔 | 500ms | editMessage 最小调用间隔 |
| Telegram 消息上限 | 4096 字符 | 超长自动截断 |
| 飞书消息上限 | 4000 字符 | |
| 钉钉消息上限 | 4000 字符 | |
| Telegram 重连最大次数 | 10 次 | 指数退避 |
| Telegram 重连最大延迟 | 30 秒 | |
| 飞书事件去重 TTL | 5 分钟 | |
| 微信响应超时 | 5 分钟 | |
| 微信最大文件大小 | 200MB | |
| 微信重试上限 | 3 次 | 连续失败后 30s 退避 |
| 工具确认超时 | 15 秒 | |

---

## 模块依赖

### 依赖

| 模块 | 依赖方式 |
|------|---------|
| `02-database` | 读写 `assistant_plugins`、`assistant_users`、`assistant_sessions`、`assistant_pairing_codes` 表 |
| `05-conversation` | 创建/查询对话、发送消息、扩展 `conversations` 表 |
| `06-ai-agent` | 通过 `WorkerTaskManager` 创建 Agent 任务，`yoloMode=true`（自动批准工具调用） |
| `07-realtime` | 事件推送（`pairing-requested`、`plugin-status-changed`、`user-authorized`）通过 WebSocket 通道 |

### 被依赖

| 模块 | 依赖方式 |
|------|---------|
| `05-conversation` | `source` 字段区分消息来源平台；`cleanupConversation` 清理通道会话 |

---

## 候选公共类型

| 类型 | 说明 | 建议归属 |
|------|------|---------|
| `PluginType` | 平台标识枚举 `'telegram' \| 'lark' \| 'dingtalk' \| 'weixin' \| ...` | `aionui-channel` |
| `PluginStatus` | 插件生命周期状态 | `aionui-channel` |
| `ChannelAgentType` | 通道 Agent 类型 `'gemini' \| 'acp' \| 'codex' \| 'openclaw-gateway'` | `aionui-common`（与 AI Agent 模块共用） |
| `IUnifiedIncomingMessage` / `IUnifiedOutgoingMessage` | 统一消息协议 | `aionui-channel` |
| `IActionButton` | 通用按钮定义 | `aionui-channel` |
| `PairingStatus` | 配对状态枚举 | `aionui-channel` |

---

## 设计决策

1. **per-chat 隔离**：原实现用 `${userId}:${chatId}` 复合键实现隔离，同一用户在不同群聊中有独立会话。Rust 重写保留此设计，复合键作为内存缓存的查找键，数据库中 `user_id` + `chat_id` 联合索引。

2. **凭据加密**：原实现用 AES-256-CBC 加密存储在 SQLite 中。Rust 重写建议使用 `ring` 或 `aes-gcm` crate 提供 AEAD 加密，密钥来源为用户级 secret（非硬编码）。

3. **扩展插件支持**：原实现的 `PluginType` 留有 `(string & {})` 扩展口，`PluginManager` 有注册表模式。Rust 重写时建议通过 trait object (`Box<dyn ChannelPlugin>`) + 动态注册实现扩展。

4. **yoloMode**：通道场景下 Agent 执行工具调用时默认自动批准（`yoloMode=true`），无需用户手动确认。仅在工具明确标记为需要确认时才通过 IM 向用户请求确认（15 秒超时）。

5. **消息更新策略差异**：各平台对"编辑已发消息"的支持不同（Telegram 直接编辑、飞书只能编辑卡片、钉钉用 AI Card 流式更新、微信不支持编辑）。Rust 重写时 `ChannelPlugin` trait 的 `editMessage` 方法允许各平台自行选择降级策略（如改为发送新消息）。

6. **conversations.source 字段**：区分对话来源（`aionui` 本地 / 各 IM 平台），用于前端过滤展示和清理策略。Rust 重写时建议改为枚举类型 `ConversationSource`。
