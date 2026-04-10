# 14 - 应用生命周期

## 概述

应用生命周期（App Lifecycle）：管理应用启动/停止、版本更新检查与下载、WebUI 服务（HTTP/WebSocket 服务器）的启停与认证基础设施（速率限制、CSRF、安全头、错误处理）、系统通知推送、以及运行时系统信息查询。

**源码位置**：`process/bridge/updateBridge.ts`、`process/bridge/applicationBridge.ts`、`process/bridge/applicationBridgeCore.ts`、`process/bridge/webuiBridge.ts`、`process/bridge/webuiQR.ts`、`process/bridge/notificationBridge.ts`、`process/bridge/services/WebuiService.ts`、`process/webserver/config/`、`process/webserver/middleware/`、`process/webserver/types/`、`common/update/`

> **设计决策 - 架构转换**：原实现中 WebUI 是 Electron 应用内嵌的 Express 服务器，可由用户手动启停。Rust 重写后，HTTP/WebSocket 服务器成为应用本体（独立进程），不再有"启停 WebUI"的概念。原 WebUI 的启停逻辑转化为 Rust 服务器本身的启动配置（端口、监听地址、远程访问等）。认证/安全中间件直接成为 Rust HTTP 框架的中间件层。

## 子模块划分

| 子模块 | 原始源码 | 迁移策略 |
|--------|---------|---------|
| 版本更新（手动） | `updateBridge.ts`、`common/update/` | 迁移 — GitHub Releases API 检查 + HTTP 下载 |
| 版本更新（自动） | `updateBridge.ts`（autoUpdate 部分） | 不迁移 — `electron-updater` 专属，Rust 版实现独立更新机制 |
| 应用管理（核心） | `applicationBridgeCore.ts` | 部分迁移 — 系统信息、路径查询迁移；目录迁移逻辑迁移 |
| 应用管理（Electron） | `applicationBridge.ts` | 不迁移 — 重启、DevTools、缩放因子、开机启动为 Electron 桌面端功能 |
| WebUI 服务 | `webuiBridge.ts`、`WebuiService.ts` | 转化 — WebUI 启停 → 服务器启动配置；密码管理、用户管理迁移 |
| QR 码登录 | `webuiQR.ts` | 迁移 — token 生成/验证/过期逻辑 |
| 通知 | `notificationBridge.ts` | 转化 — 系统通知 → WebSocket 推送 + 可选 webhook |
| 服务器配置 | `webserver/config/constants.ts` | 迁移 — 认证参数、速率限制参数、安全配置 |
| 安全中间件 | `webserver/middleware/security.ts` | 迁移 — 速率限制、CSRF、安全头 |
| 错误处理 | `webserver/middleware/errorHandler.ts` | 迁移 — 统一错误响应格式 |
| CSRF 客户端 | `webserver/middleware/csrfClient.ts` | 不迁移 — 浏览器端代码，由前端实现 |
| Express 类型 | `webserver/types/express.d.ts` | 不迁移 — TypeScript 类型声明，Rust 用结构体替代 |

---

## IPC 接口

### 版本更新（手动 — GitHub Releases）

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `update.check` | HTTP | `UpdateCheckRequest?` | `UpdateCheckResult` | 检查 GitHub Releases 是否有新版本。获取仓库所有 release，过滤 draft，用 semver 比较，返回最新可用版本信息 |
| `update.download` | HTTP+WS | `UpdateDownloadRequest` | `UpdateDownloadResult` | 下载指定版本的安装包。验证 URL 安全性（仅允许 GitHub 域名），后台启动下载，通过 `downloadProgress` 事件报告进度 |
| `update.download.progress` | WebSocket | — | `UpdateDownloadProgressEvent` | 下载进度推送：状态（starting/downloading/completed/error/cancelled）、已下载字节、总字节、速率、百分比 |
| `update.open` | WebSocket | `{ source?: 'menu' \| 'about' }` | — | 前端请求打开更新界面（由前端处理） |

### 版本更新（自动 — electron-updater）

> **不迁移**：以下接口依赖 `electron-updater`，仅在 Electron 桌面端可用。Rust 版本需设计独立的自动更新机制。

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `auto-update.check` | — | `{ includePrerelease?: boolean }` | `{ updateInfo?: { version, releaseDate?, releaseNotes? } }` | 使用 electron-updater 检查更新 |
| `auto-update.download` | — | 无 | `void` | 下载已发现的更新 |
| `auto-update.quit-and-install` | — | 无 | `void` | 退出并安装已下载的更新 |
| `auto-update.status` | — | — | `AutoUpdateStatus` | 自动更新状态变化推送 |

### 应用管理

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `system.info` | HTTP | 无 | `SystemInfo` | 获取系统目录信息（缓存目录、工作目录、日志目录、平台、架构） |
| `app.get-path` | HTTP | `{ name: 'desktop' \| 'home' \| 'downloads' }` | `string` | 获取系统标准目录路径 |
| `system.update-info` | HTTP | `{ cacheDir: string, workDir: string }` | `void` | 更新系统目录配置。规范化路径，迁移旧目录数据到新位置 |
| `app.log-stream` | WebSocket | — | `LogEntry` | 实时日志流推送（级别 + 标签 + 消息 + 可选数据） |

> **不迁移**：以下接口为 Electron 桌面端专属功能。

| 通道名 | 说明 |
|--------|------|
| `restart-app` | 重启应用（`app.relaunch()` + `app.exit(0)`） |
| `open-dev-tools` / `is-dev-tools-opened` | 开发者工具控制 |
| `app.devtools-state-changed` | DevTools 状态变化事件 |
| `app.get-zoom-factor` / `app.set-zoom-factor` | 页面缩放因子（持久化到配置） |
| `app.get-cdp-status` / `app.update-cdp-config` | Chrome DevTools Protocol 调试端口管理 |
| `app.get-start-on-boot-status` / `app.set-start-on-boot` | 开机自启动（macOS/Windows 打包版） |

### WebUI 服务管理

> **设计决策**：原实现中 WebUI 可由用户手动启停。Rust 重写后服务器即应用本体，以下接口语义转化为"服务器运行时配置管理"。

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `webui.get-status` | HTTP | 无 | `WebUIStatus` | 获取服务器运行状态：端口、监听地址、远程访问、管理员用户名 |
| `webui.start` | HTTP | `{ port?: number, allowRemote?: boolean }` | `WebUIStartResult` | 启动/重启服务器。端口被占用时自动递增（最多 +10），返回实际端口、URL、初始密码 |
| `webui.stop` | HTTP | 无 | `void` | 停止服务器。优雅关闭：先关闭所有 WebSocket 连接（发送 1000 关闭码），再关闭 HTTP 服务器 |
| `webui.change-password` | HTTP | `{ newPassword: string }` | `void` | 修改管理员密码（无需当前密码验证，从桌面端调用）。校验密码强度，更新哈希，撤销所有现有 token |
| `webui.change-username` | HTTP | `{ newUsername: string }` | `{ username: string }` | 修改管理员用户名 |
| `webui.reset-password` | HTTP | 无 | `{ newPassword: string }` | 重置管理员密码为随机生成的新密码。撤销所有现有 token |
| `webui.generate-qr-token` | HTTP | 无 | `QRTokenResult` | 生成 QR 码登录 token（5 分钟有效期），返回 token、过期时间、QR URL |
| `webui.verify-qr-token` | HTTP | `{ qrToken: string }` | `{ sessionToken: string, username: string }` | 验证 QR token 并创建会话。一次性使用，支持本地网络限制 |
| `webui.status-changed` | WebSocket | — | `WebUIStatusEvent` | 服务器状态变化推送（启动/停止） |
| `webui.reset-password-result` | WebSocket | — | `{ success: boolean, newPassword?: string, msg?: string }` | 密码重置结果推送 |

### 通知

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `notification.show` | WebSocket | `NotificationOptions` | `void` | 发送通知。检查用户是否启用通知（`system.notificationEnabled` 配置），启用时推送给客户端 |
| `notification.clicked` | WebSocket | — | `{ conversationId?: string }` | 用户点击通知后的回调事件（前端据此导航到对应会话） |

> **设计决策**：原实现通过 Electron `Notification` API 发送系统级通知。Rust 重写后改为 WebSocket 推送给所有已连接客户端，由前端决定如何展示（浏览器 Notification API / 自定义 Toast）。可选增加 webhook 通知能力（用于无人值守场景）。

---

## REST API 端点

### 认证端点

> 以下端点在原实现中由 `authRoutes.ts` 注册。认证模块已在 `03-auth.md` 中详细描述，此处仅列出与应用生命周期相关的基础设施部分。

| 方法 | 路径 | 中间件 | 功能语义 |
|------|------|--------|---------|
| POST | `/login` | `authRateLimiter` | 用户登录。常数时间密码验证（防时序攻击），设置 HTTP-only cookie |
| POST | `/logout` | `requireAuth` | 登出。将当前 token 加入黑名单，清除 session cookie |
| GET | `/api/auth/status` | — | 获取认证系统状态（是否需要初始化、用户数、是否已认证） |
| GET | `/api/auth/user` | `requireAuth` | 获取当前认证用户信息 |
| POST | `/api/auth/change-password` | `requireAuth`, `authenticatedActionLimiter` | 修改密码（需当前密码验证，与 IPC `webui.change-password` 不同） |
| POST | `/api/auth/refresh` | — | 刷新 JWT token |
| GET | `/api/ws-token` | `requireAuth` | 获取 WebSocket 认证 token（当前复用 session token） |
| GET | `/qr-login` | — | QR 码登录页面（静态 HTML） |
| POST | `/api/auth/qr-login` | `authRateLimiter` | 验证 QR token 并创建会话 |

### 文件操作端点

| 方法 | 路径 | 中间件 | 功能语义 |
|------|------|--------|---------|
| GET | `/api/directory/*` | `requireAuth`, `apiRateLimiter` | 文件浏览 API（目录遍历、文件搜索） |
| POST | `/api/upload` | `requireAuth`, `fileOperationLimiter` | 上传文件到工作区。最大 30MB，文件名清理（防路径遍历），工作区边界校验 |

---

## 核心流程

### QR 码登录流程

```
桌面端用户点击"生成 QR 码"
    ↓
webui.generateQRToken
    ├─ 生成 32 字节随机 token（crypto.randomBytes）
    ├─ 存储到内存 Map（5 分钟 TTL，标记 allowLocalOnly）
    ├─ 构建 QR URL：{baseUrl}/qr-login?token={token}
    │   ├─ allowRemote=true 且有局域网 IP → baseUrl = http://{lanIP}:{port}
    │   └─ 否则 → baseUrl = http://localhost:{port}
    └─ 返回 { token, expiresAt, qrUrl }
    ↓
桌面端展示 QR 码（编码 qrUrl）
    ↓
移动端/其他设备扫码 → 打开 GET /qr-login?token=xxx
    ↓
浏览器加载静态 HTML 页面
    ├─ 使用 textContent（非 innerHTML）展示 token（防 XSS）
    └─ 自动提交 POST /api/auth/qr-login { qrToken: token }
    ↓
服务器验证 QR token：
    ├─ token 不存在 → 401 "Invalid or expired QR token"
    ├─ token 已过期 → 401 "QR token has expired"
    ├─ token 已使用 → 401 "QR token has already been used"
    ├─ allowLocalOnly 且 clientIP 非局域网 → 403 "QR login is only allowed from local network"
    └─ 验证通过：
        ├─ 标记 token 为已使用
        ├─ 获取管理员用户
        ├─ 生成 JWT session token
        ├─ 更新最后登录时间
        ├─ 删除已用 token
        └─ 返回 { sessionToken, username }
```

### 手动更新检查与下载流程

```
前端发起 update.check({ includePrerelease? })
    ↓
resolveRepo()
    ├─ 优先使用环境变量 AIONUI_GITHUB_REPO
    └─ 默认 "iOfficeAI/AionUi"
    ↓
fetchGitHubReleases(repo)
    ├─ 调用 GitHub API: GET /repos/{owner}/{repo}/releases
    └─ 返回所有 releases
    ↓
过滤与比较：
    ├─ 排除 draft 版本
    ├─ 根据 includePrerelease 过滤 prerelease
    ├─ semver 比较 current vs latest
    └─ 选择当前平台推荐的 asset（按 OS + arch 匹配）
    ↓
返回 UpdateCheckResult { currentVersion, updateAvailable, latest? }
    ↓
如果有更新，前端选择下载：
    update.download({ url, fileName? })
        ↓
    URL 安全检查（仅允许 GitHub 相关域名）
        ↓
    后台下载：
        ├─ 生成 downloadId
        ├─ 手动处理重定向（最多 8 次）
        ├─ 文件名清理（去除 %00 等危险字符，防路径遍历）
        ├─ 通过 update.download.progress 持续推送进度：
        │   { downloadId, status, receivedBytes, totalBytes, percent, bytesPerSecond }
        └─ 下载完成返回 { downloadId, filePath }
```

### 服务器安全中间件流程

```
HTTP 请求到达
    ↓
安全头注入：
    ├─ X-Frame-Options: DENY（防点击劫持）
    ├─ X-Content-Type-Options: nosniff（防内容嗅探）
    ├─ X-XSS-Protection: 1; mode=block
    ├─ Referrer-Policy: strict-origin-when-cross-origin
    └─ Content-Security-Policy（区分 dev/prod）
    ↓
CSRF 保护：
    ├─ 生成 CSRF token 存入 cookie（aionui-csrf-token）
    ├─ 非 GET/HEAD/OPTIONS 请求必须携带 x-csrf-token 头
    └─ 校验 header token 与 cookie token 一致
    ↓
速率限制（按端点分级）：
    ├─ 认证端点：5 次 / 15 分钟（成功请求不计数）
    ├─ 通用 API：60 次 / 分钟
    ├─ 文件操作：30 次 / 分钟
    └─ 已认证操作：20 次 / 分钟（按 userId 或 IP 计数）
    ↓
JWT 认证（需要认证的端点）：
    ├─ 优先从 cookie（aionui-session）读取 token
    ├─ 备选从 Authorization: Bearer 头读取
    ├─ 验证 token 有效性 + 是否在黑名单中
    └─ 注入 req.user = { id, username }
    ↓
路由处理器
    ↓
错误处理中间件：
    ├─ AppError 实例 → 使用自定义 statusCode 和 code
    └─ 未知错误 → 500 + "Internal server error"（不暴露内部细节）
    ↓
响应格式：
    成功: { success: true, data?, message? }
    失败: { success: false, error: string, code: string }
```

---

## 数据模型

### 版本更新

```
UpdateCheckRequest {
  includePrerelease?: boolean    // 是否包含预发布版本
  repo?: string                  // GitHub 仓库（默认 "iOfficeAI/AionUi"）
}

UpdateCheckResult {
  currentVersion: string         // 当前版本
  updateAvailable: boolean       // 是否有可用更新
  latest?: UpdateReleaseInfo     // 最新版本信息
}

UpdateReleaseInfo {
  tagName: string                // Git tag（如 "v1.2.3"）
  version: string                // semver 版本号
  name?: string                  // Release 标题
  body?: string                  // Release notes（Markdown）
  htmlUrl: string                // GitHub Release 页面 URL
  publishedAt?: string           // 发布时间（ISO 8601）
  prerelease: boolean
  draft: boolean
  assets: GitHubReleaseAsset[]   // 附件列表
  recommendedAsset?: GitHubReleaseAsset  // 当前平台推荐的安装包
}

GitHubReleaseAsset {
  name: string                   // 文件名
  url: string                    // 下载 URL
  size: number                   // 文件大小（字节）
  contentType?: string           // MIME 类型
}

UpdateDownloadRequest {
  url: string                    // 下载链接（必须为 GitHub 域名）
  fileName?: string              // 自定义保存文件名
}

UpdateDownloadResult {
  downloadId: string             // 下载追踪 ID
  filePath: string               // 本地保存路径
}

UpdateDownloadProgressEvent {
  downloadId: string
  status: UpdateDownloadStatus   // 'starting' | 'downloading' | 'completed' | 'error' | 'cancelled'
  receivedBytes: number
  totalBytes?: number
  percent?: number               // 0-100
  bytesPerSecond?: number        // 下载速率
  filePath?: string              // 下载完成时的文件路径
  error?: string                 // 错误描述
}
```

### 版本信息模型

```
VersionInfo {
  current: string                // 当前版本（semver）
  latest: string                 // 最新版本（semver）
  minimumRequired?: string       // 最低要求版本（用于强制更新）
  releaseNotes?: string

  // 查询
  isUpdateAvailable: boolean     // latest > current
  isForced: boolean              // current < minimumRequired
  getUpdateType(): 'major' | 'minor' | 'patch' | 'none'
  isBreakingUpdate(): boolean    // major 版本变更
  satisfiesMinimumVersion(): boolean

  // 工厂方法
  static create(json): VersionInfo
  static isValidVersion(v): boolean
  static compareVersions(a, b): number  // -1 | 0 | 1

  // 链式
  withLatestVersion(latest, notes?): VersionInfo
  afterUpgrade(current): VersionInfo
}
```

### 系统信息

```
SystemInfo {
  cacheDir: string               // 缓存目录
  workDir: string                // 工作目录
  logDir: string                 // 日志目录
  platform: string               // 操作系统（'darwin' | 'win32' | 'linux'）
  arch: string                   // CPU 架构（'x64' | 'arm64'）
}
```

### WebUI 状态

```
WebUIStatus {
  running: boolean               // 服务器是否运行中
  port: number                   // 监听端口
  allowRemote: boolean           // 是否允许远程访问
  localUrl: string               // 本地访问 URL
  networkUrl?: string            // 局域网访问 URL（仅 allowRemote 时）
  lanIP?: string                 // 局域网 IP
  adminUsername: string           // 管理员用户名
  initialPassword?: string       // 初始密码（仅首次启动时存在）
}

WebUIStartResult {
  port: number
  localUrl: string
  networkUrl?: string
  lanIP?: string
  initialPassword?: string
}
```

### QR 码登录

```
QRTokenResult {
  token: string                  // 32 字节 hex token
  expiresAt: number              // 过期时间戳（ms）
  qrUrl: string                  // 完整的 QR 登录 URL
}

QRTokenRecord {                  // 内部存储（内存 Map）
  expiresAt: number
  used: boolean
  allowLocalOnly: boolean        // 是否限制本地网络
}
```

### 通知

```
NotificationOptions {
  title: string
  body: string
  conversationId?: string        // 关联的会话 ID（用于点击导航）
}

LogEntry {
  level: 'log' | 'warn' | 'error'
  tag: string                    // 日志来源标签
  message: string
  data?: any                     // 附加数据
}
```

### 错误响应

```
AppError {
  message: string
  statusCode: number             // HTTP 状态码（默认 500）
  code: string                   // 错误代码（默认 'internal_error'）
}

ErrorResponse {
  success: false
  error: string                  // 错误消息（用户可见，不暴露内部细节）
  code: string                   // 错误代码
}
```

---

## 关键常量

### 服务器配置

| 常量 | 值 | 说明 |
|------|-----|------|
| `DEFAULT_HOST` | `'127.0.0.1'` | 默认监听地址（仅本地） |
| `REMOTE_HOST` | `'0.0.0.0'` | 远程模式监听地址 |
| `DEFAULT_PORT` | `25808` | 默认服务端口 |
| `BODY_LIMIT` | `'10mb'` | 请求体大小限制 |
| `UPLOAD_MAX_SIZE` | `30MB` | 文件上传大小限制 |

### 认证参数

| 常量 | 值 | 说明 |
|------|-----|------|
| `SESSION_EXPIRY` | `'24h'` | 会话 token 有效期 |
| `WEBSOCKET_EXPIRY` | `'5m'` | WebSocket token 有效期 |
| `COOKIE_MAX_AGE` | `30 天` | Cookie 最大存活时间 |
| `COOKIE_NAME` | `'aionui-session'` | Session cookie 名称 |
| `COOKIE_HTTP_ONLY` | `true` | Cookie 禁止 JS 访问 |
| `COOKIE_SAME_SITE` | `'strict'`（本地）/ `'lax'`（远程 HTTP） | Cookie SameSite 策略 |

### 速率限制

| 端点类型 | 窗口 | 最大次数 | 特殊行为 |
|---------|------|---------|---------|
| 认证（登录） | 15 分钟 | 5 次 | 成功请求不计数 |
| 注册 | 15 分钟 | 3 次 | — |
| 通用 API | 1 分钟 | 60 次 | — |
| 文件操作 | 1 分钟 | 30 次 | — |
| 已认证操作 | 1 分钟 | 20 次 | 按 userId 优先，否则按 IP |

### 安全头

| Header | 值 | 作用 |
|--------|-----|------|
| `X-Frame-Options` | `DENY` | 防点击劫持 |
| `X-Content-Type-Options` | `nosniff` | 防 MIME 类型嗅探 |
| `X-XSS-Protection` | `1; mode=block` | XSS 过滤 |
| `Referrer-Policy` | `strict-origin-when-cross-origin` | 控制 Referer 泄露 |
| `Content-Security-Policy` | 按环境动态生成 | 内容安全策略 |

### CSRF 配置

| 常量 | 值 | 说明 |
|------|-----|------|
| `CSRF_COOKIE_NAME` | `'aionui-csrf-token'` | CSRF token cookie 名称 |
| `CSRF_HEADER_NAME` | `'x-csrf-token'` | CSRF token 请求头名称 |
| `CSRF_TOKEN_LENGTH` | `32` | token 长度（字节） |

### QR 码登录

| 常量 | 值 | 说明 |
|------|-----|------|
| QR token 有效期 | `5 分钟` | 过期自动失效 |
| QR token 长度 | `32 字节 hex` | 64 字符十六进制字符串 |
| 一次性使用 | `true` | 验证后立即删除 |

### WebSocket

| 常量 | 值 | 说明 |
|------|-----|------|
| `HEARTBEAT_INTERVAL` | `30s` | 心跳发送间隔 |
| `HEARTBEAT_TIMEOUT` | `60s` | 无心跳响应超时 |
| `CLOSE_CODE_NORMAL` | `1000` | 正常关闭 |
| `CLOSE_CODE_POLICY` | `1008` | 策略违规关闭 |

### 更新下载安全

| 常量 | 值 | 说明 |
|------|-----|------|
| 最大重定向次数 | `8` | 防无限重定向 |
| URL 白名单 | GitHub 相关域名 | 仅允许从 GitHub 下载 |

---

## 局域网 IP 检测

QR 码登录和远程访问功能依赖局域网 IP 检测和判断：

```
isLocalIP(ip) 判定规则：
  ├─ 先处理 IPv6 映射格式：去除 "::ffff:" 前缀
  ├─ localhost: '127.0.0.1', 'localhost', '::1' → true
  ├─ A 类私有网络: 10.0.0.0/8 → true
  ├─ B 类私有网络: 172.16.0.0/12 → true
  ├─ C 类私有网络: 192.168.0.0/16 → true
  ├─ 链路本地: 169.254.0.0/16 → true
  └─ 其他 → false

getLanIP() 获取规则：
  ├─ 遍历所有网络接口
  ├─ 过滤条件：非 internal + IPv4 + 非 127.0.0.1
  └─ 返回第一个匹配的 IP
```

---

## 与其他模块的集成

### 依赖

| 模块 | 依赖方式 |
|------|---------|
| `02-database` | 用户数据存储（密码哈希、最后登录时间） |
| `03-auth` | JWT token 生成/验证、密码哈希/校验、token 黑名单 |
| `04-system-settings` | 读取 `system.notificationEnabled` 配置、`ui.zoomFactor` 配置 |

### 被依赖

| 模块 | 依赖方式 |
|------|---------|
| 所有模块 | 认证中间件、安全头、速率限制、错误处理 — 所有 HTTP 端点共享此基础设施 |
| `07-realtime` | WebSocket 心跳配置、认证 token 验证 |
| `05-conversation` | 通知推送（新消息通知） |
| `09-channel` | 通知推送（通道消息通知） |

---

## 外部依赖

| 库 | 用途 | Rust 替代建议 |
|----|------|--------------|
| `express` | HTTP 服务器 | `axum` 或 `actix-web` |
| `ws` | WebSocket 服务器 | `axum` 内置 WebSocket 或 `tokio-tungstenite` |
| `jsonwebtoken` | JWT 生成与验证 | `jsonwebtoken` crate |
| `bcrypt` | 密码哈希 | `argon2` crate（更安全）或 `bcrypt` crate |
| `express-rate-limit` | 速率限制 | `tower-governor` 或自定义 `tower::Service` |
| `tiny-csrf` | CSRF 保护 | 自定义中间件（Double Submit Cookie 模式） |
| `multer` | 文件上传 | `axum::extract::Multipart` |
| `helmet`（推测） | 安全头 | 自定义 `tower::Layer` |
| `semver` | 版本比较 | `semver` crate |
| `electron-updater` | 自动更新 | 不适用（Rust 需独立方案） |
| `node-fetch` / `https` | GitHub API 调用 | `reqwest` crate |

---

## 设计决策

1. **WebUI → 原生服务器**：原实现中 WebUI 是可选的内嵌服务器，用户可手动启停。Rust 重写后，HTTP/WebSocket 服务器就是应用本身。`webui.start/stop` 不再作为运行时 API 暴露，改为启动时配置（命令行参数或配置文件指定端口、监听地址、是否允许远程访问）。管理员密码管理、QR 码登录等功能保留。

2. **通知模型转换**：原实现通过 Electron `Notification` API 发送桌面系统通知。Rust 版本改为：
   - **WebSocket 推送**：通知作为消息事件发送给所有已连接客户端
   - **前端自行展示**：使用浏览器 Notification API 或自定义 UI
   - **可选 Webhook**：配置 webhook URL，用于服务器无人值守时的通知转发

3. **双通道密码修改**：原实现有两种密码修改路径：
   - IPC `webui.change-password`：桌面端调用，无需当前密码验证（因为已通过桌面端认证）
   - REST `POST /api/auth/change-password`：Web 端调用，需要当前密码验证
   
   Rust 重写后统一为 REST 端点，始终需要当前密码验证（或提供管理员级别的重置端点）。

4. **CSRF 策略**：原实现使用 Double Submit Cookie 模式（token 同时存在于 cookie 和 header）。Rust 重写时保持此策略，但可考虑对纯 API 调用（非浏览器客户端）使用 Bearer token 认证替代 CSRF。

5. **速率限制存储**：原实现使用内存存储（`express-rate-limit` 默认）。单实例部署可保持内存存储；若未来需要多实例，可迁移到 Redis 或 SQLite 存储。

6. **更新机制分离**：
   - **自动更新**（`electron-updater`）不迁移，由桌面端 Electron 薄层继续负责
   - **手动更新检查**（GitHub Releases API）迁移到 Rust，用于非 Electron 部署场景（如 Docker / 独立二进制）
   - 两者可共存：Electron 版使用自动更新，独立版使用手动检查

7. **QR 码 token 存储**：原实现使用内存 Map 存储 QR token（5 分钟 TTL）。考虑到 QR token 短寿命且仅用于一次性认证，内存存储足够。Rust 重写时使用 `DashMap` 或 `tokio::sync::RwLock<HashMap>` + 定期清理过期 token 的后台任务。

8. **错误响应格式统一**：原实现使用 `AppError` 类 + 错误处理中间件统一错误格式。Rust 重写时在 `aionui-common` 中定义统一的 `AppError` 枚举（实现 `IntoResponse`），所有 crate 共享。生产环境不暴露内部错误细节。

---

## 候选公共类型

| 类型 | 说明 | 建议归属 |
|------|------|---------|
| `AppError` | 统一错误类型（statusCode + code + message） | `aionui-common` |
| `ErrorResponse` | HTTP 错误响应格式 | `aionui-api-types` |
| `UpdateCheckRequest` | 更新检查请求 | `aionui-api-types` |
| `UpdateCheckResult` | 更新检查结果 | `aionui-api-types` |
| `UpdateReleaseInfo` | 版本发布信息 | `aionui-api-types` |
| `GitHubReleaseAsset` | GitHub Release 附件 | `aionui-system`（内部类型） |
| `UpdateDownloadProgressEvent` | 下载进度事件 | `aionui-api-types` |
| `VersionInfo` | 版本信息模型（含比较逻辑） | `aionui-common` |
| `SystemInfo` | 系统目录与平台信息 | `aionui-api-types` |
| `WebUIStatus` | 服务器运行状态 | `aionui-api-types` |
| `QRTokenResult` | QR 码登录 token | `aionui-api-types` |
| `NotificationOptions` | 通知选项 | `aionui-api-types` |
| `LogEntry` | 日志条目 | `aionui-common` |
| `RateLimitConfig` | 速率限制配置 | `aionui-system`（内部配置） |
