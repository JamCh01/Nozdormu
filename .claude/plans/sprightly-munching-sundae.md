# NetPulse Frontend — UI Design Document

> 在开始编码之前，先对所有页面的布局、组件结构、交互逻辑做完整设计。

---

## 1. Design System & Global Style

**色彩体系 (Dark-first, 支持 Light):**
- Primary: Blue-500 (#3B82F6)
- Success/Online: Green-500 (#22C55E)
- Warning: Amber-500 (#F59E0B)
- Danger/Offline: Red-500 (#EF4444)
- Disabled: Gray-400 (#9CA3AF)
- Background (Dark): Slate-950 → Slate-900
- Background (Light): White → Gray-50
- Surface (Dark): Slate-800/80 with subtle border
- Surface (Light): White with gray-200 border

**字体:** Inter (UI) / JetBrains Mono (数值/代码)

**圆角:** 组件统一 `rounded-lg` (8px)，卡片 `rounded-xl` (12px)

**间距:** 基于 4px grid (Tailwind 默认)

**状态 Badge 颜色映射:**
| 状态 | Badge 样式 |
|------|-----------|
| online | `bg-green-500/10 text-green-500 border-green-500/20` |
| offline | `bg-gray-500/10 text-gray-400 border-gray-500/20` |
| disabled | `bg-red-500/10 text-red-500 border-red-500/20` |
| active | `bg-green-500/10 text-green-500` |
| inactive | `bg-gray-500/10 text-gray-400` |
| firing | `bg-red-500/10 text-red-500 animate-pulse` |
| resolved | `bg-green-500/10 text-green-500` |

**协议 Badge:**
| 协议 | Badge |
|------|-------|
| ICMP | `bg-blue-500/10 text-blue-400` |
| TCP  | `bg-purple-500/10 text-purple-400` |
| UDP  | `bg-amber-500/10 text-amber-400` |
| HTTP | `bg-emerald-500/10 text-emerald-400` |

---

## 2. App Layout

### 2.1 Sidebar (左侧固定)

```
┌──────────────────────────────────────────────────────────┐
│ ┌─────────┐                                              │
│ │         │                                              │
│ │ SIDEBAR │              MAIN CONTENT                    │
│ │  240px  │                                              │
│ │(折叠56px)│                                              │
│ │         │                                              │
│ └─────────┘                                              │
└──────────────────────────────────────────────────────────┘
```

**Sidebar 完整布局:**
```
┌────────────────────────┐
│  ⚡ NetPulse      [<>] │  ← Logo + 折叠按钮
├────────────────────────┤
│                        │
│  📊 Dashboard          │  ← 所有角色可见
│  📋 Tasks              │
│  📈 Monitoring         │  ← (作为 Tasks 子导航，不在sidebar)
│                        │
│  ─────────────────     │  ← 分隔线
│  🤖 Agents        🔒   │  ← Admin Only (锁图标)
│  👥 Users         🔒   │
│                        │
│  ─────────────────     │
│  🔔 Alerts             │  ← 所有角色可见
│  🔗 Webhooks           │
│                        │
│                        │
│  ─────────────────     │
│  ⚙️ Settings           │  ← 底部固定
│  🚪 Logout             │
└────────────────────────┘
```

**折叠态:** 只显示图标，hover 显示 tooltip

### 2.2 Header (顶部)

```
┌──────────────────────────────────────────────────────────┐
│  Breadcrumb: Dashboard > ...        🌙/☀️  👤 username ▾ │
│                                           ┌────────────┐│
│                                           │ Profile     ││
│                                           │ Settings    ││
│                                           │ ──────────  ││
│                                           │ Logout      ││
│                                           └────────────┘│
└──────────────────────────────────────────────────────────┘
```

---

## 3. Auth Pages (未登录)

### 3.1 Login Page

```
┌──────────────────────────────────────────────────────────┐
│                                                          │
│                    ⚡ NetPulse                            │
│              Network Monitoring System                   │
│                                                          │
│              ┌─────────────────────┐                     │
│              │                     │                     │
│              │  Username           │                     │
│              │  ┌─────────────┐    │                     │
│              │  │             │    │                     │
│              │  └─────────────┘    │                     │
│              │                     │                     │
│              │  Password           │                     │
│              │  ┌─────────────┐    │                     │
│              │  │          👁  │    │                     │
│              │  └─────────────┘    │                     │
│              │                     │                     │
│              │  ┌─────────────┐    │                     │
│              │  │   Sign In   │    │  ← Primary Button   │
│              │  └─────────────┘    │                     │
│              │                     │                     │
│              │  Don't have an      │                     │
│              │  account? Register  │  ← Link             │
│              │                     │                     │
│              └─────────────────────┘                     │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

**交互:** 
- 表单校验: username 必填, password 必填 (min 8 chars)
- 提交后 Loading 状态 → 成功跳转 /dashboard → 失败显示 toast 错误
- 回车键提交

### 3.2 Register Page

```
┌──────────────────────────────────────────────────────────┐
│                    ⚡ NetPulse                            │
│              Create your account                         │
│                                                          │
│              ┌─────────────────────┐                     │
│              │  Username           │                     │
│              │  ┌─────────────┐    │                     │
│              │  │             │    │  ← 2-64 chars       │
│              │  └─────────────┘    │                     │
│              │                     │                     │
│              │  Email              │                     │
│              │  ┌─────────────┐    │                     │
│              │  │             │    │  ← email format     │
│              │  └─────────────┘    │                     │
│              │                     │                     │
│              │  Password           │                     │
│              │  ┌─────────────┐    │                     │
│              │  │          👁  │    │  ← 8-128 chars     │
│              │  └─────────────┘    │                     │
│              │                     │                     │
│              │  Role               │                     │
│              │  ┌─────────────┐    │                     │
│              │  │ Subscriber ▾│    │  ← 下拉: admin /    │
│              │  └─────────────┘    │    subscriber       │
│              │                     │                     │
│              │  ┌─────────────┐    │                     │
│              │  │   Register  │    │                     │
│              │  └─────────────┘    │                     │
│              │                     │                     │
│              │  Already have an    │                     │
│              │  account? Sign in   │                     │
│              └─────────────────────┘                     │
└──────────────────────────────────────────────────────────┘
```

---

## 4. Dashboard Page

```
┌──────────────────────────────────────────────────────────┐
│  Dashboard                                               │
│                                                          │
│  ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────┐│
│  │ 🟢 Online  │ │ ⚫ Offline │ │ 🔴 Disabled│ │ Active ││
│  │   Agents   │ │   Agents   │ │   Agents   │ │ Tasks  ││
│  │            │ │            │ │            │ │        ││
│  │    10      │ │     3      │ │     1      │ │   25   ││
│  │  / 14 total│ │            │ │            │ │ /30    ││
│  └────────────┘ └────────────┘ └────────────┘ └────────┘│
│                                                          │
│  Active Tasks                                            │
│  ┌──────────────────┐ ┌──────────────────┐ ┌───────────┐│
│  │ Ping Google DNS  │ │ HTTP api.example │ │ TCP DB    ││
│  │ ICMP 8.8.8.8     │ │ HTTP :443        │ │ TCP :5432 ││
│  │ ┌──────────────┐ │ │ ┌──────────────┐ │ │ ┌───────┐ ││
│  │ │ ~~~~~~~~~~~~ │ │ │ │ ~~~~~~~~~~~~ │ │ │ │ ~~~~~ │ ││
│  │ │ mini chart   │ │ │ │ mini chart   │ │ │ │       │ ││
│  │ │ 120px height │ │ │ │ median+band  │ │ │ │       │ ││
│  │ └──────────────┘ │ │ └──────────────┘ │ │ └───────┘ ││
│  │ Median: 5.3ms    │ │ Median: 42.1ms   │ │ 1.2ms    ││
│  │ Loss: 0.0%       │ │ Loss: 0.1%       │ │ 0.0%     ││
│  └──────────────────┘ └──────────────────┘ └───────────┘│
│                                                          │
│  (grid-cols-1 md:grid-cols-2 xl:grid-cols-3)             │
│  (Intersection Observer 懒加载视口外图表)                  │
└──────────────────────────────────────────────────────────┘
```

**交互:**
- 统计卡片实时刷新 (staleTime 5min)
- 迷你图表：仅 median 折线 + min~max band, 无 tooltip
- 点击任务卡片 → 跳转 `/monitoring/:taskUuid`
- Loading 时显示 Skeleton 占位

---

## 5. Monitoring Pages

### 5.1 Monitoring Detail Page (`/monitoring/:taskUuid`)

```
┌──────────────────────────────────────────────────────────┐
│  ← Back to Tasks    Ping Google DNS                      │
│                     ICMP → 8.8.8.8          [Compare]    │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │ [1h] [6h] [24h] [7d] [30d] [1y] [Custom ▾]         ││
│  │                                     Granularity: raw ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  Agent: ┌──────────────────┐                             │
│         │ All Agents    ▾  │  ← 下拉: 关联的 Agent 列表  │
│         └──────────────────┘                             │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │                                                      ││
│  │     SmokePing Style Chart (400px height)              ││
│  │                                                      ││
│  │  ms                                                  ││
│  │  ▲                                                   ││
│  │  │    ████████████████████████                        ││
│  │  │  ██░░░░░░░░░░░░░░░░░░░░░░██    ← p99~max band    ││
│  │  │ █░░▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░█    ← p95~p99 band   ││
│  │  │█░▓▓▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▓▓░░░█    ← avg~p95 band    ││
│  │  │█▓▓▒▒░░░░░░░░░░░░░░▒▒▓▓░░█    ← min~avg band    ││
│  │  │█▓▒░──────median───────░▒▓█    ← median 折线      ││
│  │  │                                                   ││
│  │  │  ▓▓▓ packet loss highlight ▓▓▓  ← 丢包红色标记    ││
│  │  │                                                   ││
│  │  └───────────────────────────────▶ time              ││
│  │                                                      ││
│  │  Tooltip (hover):                                    ││
│  │  ┌─────────────────────┐                             ││
│  │  │ 2026-04-03 14:30    │                             ││
│  │  │ Median:  5.3 ms     │                             ││
│  │  │ Avg:     6.1 ms     │                             ││
│  │  │ Min/Max: 1.2/15.6ms │                             ││
│  │  │ P95:     12.1 ms    │                             ││
│  │  │ P99:     14.9 ms    │                             ││
│  │  │ Loss:    0.0%       │                             ││
│  │  └─────────────────────┘                             ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  Raw Data  [Expand ▾]                                    │
│  ┌──────────────────────────────────────────────────────┐│
│  │ Timestamp      │ Median │ Avg  │ Min │ Max │ Loss   ││
│  │────────────────│────────│──────│─────│─────│────────││
│  │ 04-03 14:30    │ 5.3ms  │ 6.1  │ 1.2 │15.6 │ 0.0%  ││
│  │ 04-03 14:29    │ 4.8ms  │ 5.9  │ 1.0 │14.2 │ 0.0%  ││
│  │ ...            │ ...    │ ...  │ ... │ ... │ ...    ││
│  └──────────────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────┘
```

**SmokePing 图表 Band 颜色 (Dark theme):**
- min~avg: `rgba(59, 130, 246, 0.1)` — 最浅蓝
- avg~p95: `rgba(59, 130, 246, 0.25)` — 浅蓝
- p95~p99: `rgba(59, 130, 246, 0.4)` — 中蓝
- p99~max: `rgba(59, 130, 246, 0.55)` — 深蓝
- Median line: `#3B82F6` solid, smooth, 2px
- Packet loss: `rgba(239, 68, 68, 0.3)` markArea

### 5.2 Compare Page (`/monitoring/:taskUuid/compare`)

```
┌──────────────────────────────────────────────────────────┐
│  ← Back     Compare: Ping Google DNS                     │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │ [1h] [6h] [24h] [7d] [30d] [1y]                     ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ┌─────────────────────┐ ┌─────────────────────┐        │
│  │ Agent A:  [FRA-01▾] │ │ Agent B:  [NYC-01▾] │        │
│  ├─────────────────────┤ ├─────────────────────┤        │
│  │                     │ │                     │        │
│  │  SmokePing Chart A  │ │  SmokePing Chart B  │        │
│  │  (linked zoom/tip)  │ │  (linked zoom/tip)  │        │
│  │                     │ │                     │        │
│  └─────────────────────┘ └─────────────────────┘        │
│                                                          │
│  (ECharts connect 实现缩放/tooltip 联动)                  │
└──────────────────────────────────────────────────────────┘
```

---

## 6. Tasks Pages

### 6.1 Task List Page

```
┌──────────────────────────────────────────────────────────┐
│  Tasks                                    [+ Create Task]│
│                                        (Admin only btn)  │
│  Filter: Status ┌──────────┐                             │
│                  │ All    ▾ │  ← Active / Inactive / All │
│                  └──────────┘                             │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │ Name          │Protocol│ Target    │Port│Intv│Status │││
│  │───────────────│────────│──────────│────│────│───────│││
│  │ Ping Google   │ ICMP   │ 8.8.8.8  │ —  │ 60s│🟢     │││
│  │ HTTP API      │ HTTP   │ api.ex.. │443 │ 30s│🟢     │││
│  │ TCP Database  │ TCP    │ db.int.. │5432│ 60s│⚫     │││
│  │                                                      ││
│  │ (hover row → 显示 Monitor / Edit / Delete 操作)       ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ← 1  2  3 →                        Showing 1-20 of 30  │
└──────────────────────────────────────────────────────────┘
```

**行操作:**
- 点击行 → 跳转 Task Detail
- 📈 Monitor → 跳转 `/monitoring/:taskUuid`
- ✏️ Edit → 跳转 Task Detail (编辑模式)
- 🗑️ Delete → 确认弹窗 → 停用 (Admin only)
- Subscriber 只看到 Monitor 按钮

### 6.2 Create Task Page/Dialog

```
┌──────────────────────────────────────────────────────────┐
│  Create New Task                                         │
│                                                          │
│  Task Name                                               │
│  ┌──────────────────────────────────────────┐             │
│  │                                          │             │
│  └──────────────────────────────────────────┘             │
│                                                          │
│  Protocol                    Target                      │
│  ┌────────────┐              ┌────────────────────┐      │
│  │  ICMP    ▾ │              │ 8.8.8.8            │      │
│  └────────────┘              └────────────────────┘      │
│                                                          │
│  Port                        (隐藏 when ICMP/UDP)        │
│  ┌────────────┐              ← 仅 TCP/HTTP 时显示且必填   │
│  │  443       │                                          │
│  └────────────┘                                          │
│                                                          │
│  Interval (s)    Packet Count     Timeout (s)            │
│  ┌──────┐        ┌──────┐         ┌──────┐               │
│  │  60  │        │  20  │         │  5   │               │
│  └──────┘        └──────┘         └──────┘               │
│  min: 10         min: 1           min: 1                 │
│                                                          │
│            [Cancel]                  [Create Task]        │
└──────────────────────────────────────────────────────────┘
```

### 6.3 Task Detail Page

```
┌──────────────────────────────────────────────────────────┐
│  ← Tasks     Ping Google DNS              [📈 Monitor]   │
│                                                          │
│  ┌── Task Info ─────────────────────────────────────────┐│
│  │ Protocol: ICMP    Target: 8.8.8.8    Interval: 60s  ││
│  │ Packets: 20       Timeout: 5s        Status: 🟢      ││
│  │ Created: 2026-04-01                                  ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ┌── Edit Task (Admin only) ───────────────────────────┐│
│  │ Task Name: [Ping Google DNS    ]                     ││
│  │ Target:    [8.8.8.8            ]                     ││
│  │ Interval:  [60 ] Packets: [20 ] Active: [✓]         ││
│  │                               [Save Changes]        ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ┌── Assigned Agents (Admin only editable) ─────────────┐│
│  │                                                      ││
│  │  Assigned (3):                    Available:          ││
│  │  ┌────────────────────┐          ┌──────────────────┐││
│  │  │ FRA-01  🟢  [✕]   │          │ 🔍 Search/Filter │││
│  │  │ NYC-01  🟢  [✕]   │          │                  │││
│  │  │ TYO-01  ⚫  [✕]   │          │ SIN-01 🟢  [+]  │││
│  │  │                    │          │ LAX-01 🟢  [+]  │││
│  │  │                    │          │ CDG-01 ⚫  [+]  │││
│  │  └────────────────────┘          └──────────────────┘││
│  │                                                      ││
│  │  Tag Filter:                                         ││
│  │  Continent [All▾] Country [All▾] City [All▾] ISP [▾] ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  [🗑️ Deactivate Task]  (Admin only, 底部左侧)            │
└──────────────────────────────────────────────────────────┘
```

---

## 7. Agents Pages (Admin Only)

### 7.1 Agent List Page

```
┌──────────────────────────────────────────────────────────┐
│  Agents                                  [+ Create Agent]│
│                                                          │
│  Tag Filters:                                            │
│  Continent ┌──────┐  Country ┌──────┐  City ┌──────┐    │
│            │ All ▾│          │ All ▾│       │ All ▾│    │
│            └──────┘          └──────┘       └──────┘    │
│  ISP ┌──────┐                                           │
│      │ All ▾│  ← 每个维度独立多选过滤                     │
│      └──────┘                                           │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │ Name    │ Tags                        │Status│Created││
│  │─────────│────────────────────────────│──────│───────││
│  │ FRA-01  │ eu  german  FRA  SnapStack │ 🟢   │ 04-01 ││
│  │ NYC-01  │ na  usa     NYC  AWS       │ 🟢   │ 04-01 ││
│  │ TYO-01  │ asia japan  TYO  Vultr     │ ⚫   │ 04-02 ││
│  │ SIN-01  │ asia sg     SIN  DO        │ 🔴   │ 03-28 ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ← 1  2 →                              Showing 1-50     │
└──────────────────────────────────────────────────────────┘
```

**Tags 显示:** 每个 tag 渲染为独立 Badge, `continent:eu` → 显示为 `eu`

### 7.2 Create Agent Dialog (Modal)

```
┌──────────────────────────────────────┐
│  Create New Agent                 ✕  │
│                                      │
│  Agent Name                          │
│  ┌──────────────────────────────┐    │
│  │ FRA-02                       │    │
│  └──────────────────────────────┘    │
│                                      │
│  Tags (所有 4 个维度必填)             │
│  Continent ┌─────────────────┐       │
│            │ eu              │       │
│            └─────────────────┘       │
│  Country   ┌─────────────────┐       │
│            │ german          │       │
│            └─────────────────┘       │
│  City      ┌─────────────────┐       │
│            │ FRA             │       │
│            └─────────────────┘       │
│  ISP       ┌─────────────────┐       │
│            │ SnapStack       │       │
│            └─────────────────┘       │
│                                      │
│       [Cancel]      [Create Agent]   │
│                                      │
│  ┌── Success State ────────────────┐ │
│  │ ✅ Agent created!               │ │
│  │                                  │ │
│  │ Access Key (shown only once):    │ │
│  │ ┌──────────────────────────┐    │ │
│  │ │ sk-abc123def456...  [📋] │    │ │
│  │ │              Copy button │    │ │
│  │ └──────────────────────────┘    │ │
│  │                                  │ │
│  │ ⚠️  Save this key now.          │ │
│  │    It won't be shown again.     │ │
│  └──────────────────────────────────┘ │
└──────────────────────────────────────┘
```

### 7.3 Agent Detail Page

```
┌──────────────────────────────────────────────────────────┐
│  ← Agents     FRA-01                       Status: 🟢    │
│                                                          │
│  ┌── Agent Info ────────────────────────────────────────┐│
│  │ Tags: eu  german  FRA  SnapStack                     ││
│  │ Created: 2026-04-01 10:30                            ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ┌── Edit Agent ────────────────────────────────────────┐│
│  │ Name: [FRA-01              ]                         ││
│  │ Continent: [eu    ] Country: [german ]               ││
│  │ City:      [FRA   ] ISP:     [SnapStack]             ││
│  │                                    [Save Changes]    ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  [🗑️ Disable Agent]  ← 确认弹窗                          │
└──────────────────────────────────────────────────────────┘
```

---

## 8. Users Pages (Admin Only)

### 8.1 User List Page

```
┌──────────────────────────────────────────────────────────┐
│  Users                                                   │
│                                                          │
│  Role: ┌──────────┐                                      │
│        │ All    ▾ │  ← Admin / Subscriber / All          │
│        └──────────┘                                      │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │ Username  │ Email            │ Role       │ Active   ││
│  │───────────│─────────────────│───────────│─────────  ││
│  │ admin01   │ admin@ex.com    │ Admin     │ 🟢        ││
│  │ viewer01  │ view@ex.com     │ Subscriber│ 🟢        ││
│  │ old_user  │ old@ex.com      │ Subscriber│ ⚫        ││
│  │                                                      ││
│  │ (hover → Edit / Disable 操作)                         ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ← 1  2 →                              Showing 1-20     │
└──────────────────────────────────────────────────────────┘
```

### 8.2 User Detail/Edit Page

```
┌──────────────────────────────────────────────────────────┐
│  ← Users     admin01                                     │
│                                                          │
│  ┌── User Info ─────────────────────────────────────────┐│
│  │ Role: Admin        Active: 🟢                        ││
│  │ Email: admin@example.com                             ││
│  │ Created: 2026-04-01                                  ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ┌── Edit User ─────────────────────────────────────────┐│
│  │ Username: [admin01         ]                         ││
│  │ Email:    [admin@ex.com    ]                         ││
│  │ Active:   [✓]                                        ││
│  │                                    [Save Changes]    ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ┌── Group Membership ──────────────────────────────────┐│
│  │ Current Groups:                                      ││
│  │  ┌──────────────────────────────┐                    ││
│  │  │ Group A              [✕]    │  ← 移出按钮        ││
│  │  │ Group B              [✕]    │                    ││
│  │  └──────────────────────────────┘                    ││
│  │                                                      ││
│  │ Add to Group: ┌──────────────┐ [Add]                 ││
│  │               │ Select... ▾  │                       ││
│  │               └──────────────┘                       ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  [🗑️ Disable User]                                       │
└──────────────────────────────────────────────────────────┘
```

---

## 9. Alerts Pages

### 9.1 Alert Rules List Page

```
┌──────────────────────────────────────────────────────────┐
│  Alert Rules                             [+ Create Rule] │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │ Task       │ Metric   │ Condition     │ M/N │Status ││
│  │────────────│──────────│──────────────│─────│───────││
│  │ Ping Googl │ Latency  │ > 100ms       │ 3/5 │ 🟢    ││
│  │ HTTP API   │ PktLoss  │ > 5%          │ 2/3 │ 🟢    ││
│  │ TCP DB     │ Jitter   │ >= 20ms       │ 4/5 │ ⚫    ││
│  │                                                      ││
│  │ (Admin sees all, Subscriber sees own only)            ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  Metric Badge: Latency=蓝, Jitter=紫, Packet Loss=红    │
│  M/N: "3 of 5 checks" 格式                               │
│  Condition: operator 转可读符号 (gt→>, gte→>=, etc)       │
└──────────────────────────────────────────────────────────┘
```

### 9.2 Alert Rule Create/Edit Form

```
┌──────────────────────────────────────────────────────────┐
│  Create Alert Rule                                       │
│                                                          │
│  Task                                                    │
│  ┌──────────────────────────────────────────┐             │
│  │ Select task...                        ▾ │  ← 任务下拉 │
│  └──────────────────────────────────────────┘             │
│                                                          │
│  Metric Type          Operator           Threshold       │
│  ┌────────────┐       ┌────────────┐     ┌──────────┐    │
│  │ Latency  ▾ │       │ >  (gt)  ▾ │     │ 100      │    │
│  └────────────┘       └────────────┘     └──────────┘    │
│                                          (ms / % 单位)   │
│                                                          │
│  Alert Strategy (M of N)                                 │
│  ┌──────┐ of ┌──────┐  checks                           │
│  │  3   │    │  5   │   ← m_count <= n_count 校验       │
│  └──────┘    └──────┘                                    │
│                                                          │
│  Notify via Webhooks (optional)                          │
│  ┌──────────────────────────────────────────┐             │
│  │ ☑ Feishu Alert                           │             │
│  │ ☐ Slack Ops Channel                      │             │
│  │ ☐ PagerDuty                              │             │
│  └──────────────────────────────────────────┘             │
│                                                          │
│            [Cancel]              [Create Rule]            │
└──────────────────────────────────────────────────────────┘
```

---

## 10. Webhooks Pages

### 10.1 Webhook List Page

```
┌──────────────────────────────────────────────────────────┐
│  Webhooks                              [+ Create Webhook]│
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │ Name          │ URL                  │Method│Status  ││
│  │───────────────│─────────────────────│──────│────────││
│  │ Feishu Alert  │ hooks.feishu.cn/abc │ POST │ 🟢     ││
│  │ Slack Ops     │ hooks.slack.com/... │ POST │ 🟢     ││
│  │ PagerDuty     │ events.pager...     │ POST │ ⚫     ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  (每个用户只看到自己的 Webhook)                            │
└──────────────────────────────────────────────────────────┘
```

### 10.2 Webhook Create/Edit Form

```
┌──────────────────────────────────────────────────────────┐
│  Create Webhook                                          │
│                                                          │
│  Name                                                    │
│  ┌──────────────────────────────────────────┐             │
│  │ Feishu Alert                             │             │
│  └──────────────────────────────────────────┘             │
│                                                          │
│  URL                                                     │
│  ┌──────────────────────────────────────────┐             │
│  │ https://hooks.feishu.cn/abc              │             │
│  └──────────────────────────────────────────┘             │
│                                                          │
│  Method                                                  │
│  ┌────────────┐                                          │
│  │  POST    ▾ │  ← 默认 POST                             │
│  └────────────┘                                          │
│                                                          │
│  Custom Headers (optional)                               │
│  ┌──────────────────────────────────────────┐             │
│  │ Key              │ Value          │ [✕]  │             │
│  │──────────────────│───────────────│──────│             │
│  │ Authorization    │ Bearer tok123 │ [✕]  │             │
│  │ Content-Type     │ application.. │ [✕]  │             │
│  │                  │               │      │             │
│  │                [+ Add Header]           │             │
│  └──────────────────────────────────────────┘             │
│                                                          │
│            [Cancel]            [Create Webhook]           │
└──────────────────────────────────────────────────────────┘
```

---

## 11. 共通组件

### 11.1 确认弹窗 (Confirm Dialog)

```
┌──────────────────────────────────┐
│  ⚠️ Confirm Action            ✕  │
│                                  │
│  Are you sure you want to        │
│  disable agent "FRA-01"?         │
│  This action can be reversed.    │
│                                  │
│       [Cancel]    [Confirm]      │
│                 (红色 Danger btn) │
└──────────────────────────────────┘
```

### 11.2 Empty State

```
┌──────────────────────────────────┐
│                                  │
│          📭                      │
│    No tasks found               │
│    Create your first task to    │
│    start monitoring.            │
│                                  │
│       [+ Create Task]           │
│                                  │
└──────────────────────────────────┘
```

### 11.3 Error State

```
┌──────────────────────────────────┐
│                                  │
│          ❌                      │
│    Failed to load data          │
│    Something went wrong.        │
│                                  │
│       [🔄 Retry]                │
│                                  │
└──────────────────────────────────┘
```

### 11.4 403 Forbidden Page

```
┌──────────────────────────────────────────────────────────┐
│                                                          │
│                        🔒                                │
│                  Access Denied                           │
│                                                          │
│          You don't have permission to                    │
│          access this resource.                           │
│                                                          │
│              [← Back to Dashboard]                       │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

---

## 12. 页面路由总览

| Route | Page | 角色 |
|-------|------|------|
| `/login` | Login Page | Public |
| `/register` | Register Page | Public |
| `/dashboard` | Dashboard (统计 + 迷你图表) | All |
| `/tasks` | Task List | All (Admin CRUD, Sub readonly) |
| `/tasks/create` | Create Task | Admin |
| `/tasks/:uuid` | Task Detail + Agent Assignment | All |
| `/monitoring/:taskUuid` | Monitoring Chart | All |
| `/monitoring/:taskUuid/compare` | Compare View | All |
| `/agents` | Agent List | Admin |
| `/agents/:uuid` | Agent Detail | Admin |
| `/users` | User List | Admin |
| `/users/:uuid` | User Detail | Admin |
| `/alerts` | Alert Rules List | All |
| `/alerts/create` | Create Alert Rule | All |
| `/alerts/:uuid/edit` | Edit Alert Rule | All (own) |
| `/webhooks` | Webhook List | All (own) |
| `/webhooks/create` | Create Webhook | All |
| `/webhooks/:uuid/edit` | Edit Webhook | All (own) |

---

## 13. 响应式策略

- **Desktop (>=1280px):** Sidebar 展开 240px + 完整内容区
- **Tablet (768-1279px):** Sidebar 折叠为图标模式 56px
- **Mobile (<768px):** Sidebar 隐藏，顶部 Hamburger 菜单触发 Drawer
- Dashboard 网格: `grid-cols-1 md:grid-cols-2 xl:grid-cols-3`
- Compare 页面: 平板/桌面并排, 手机上下堆叠

---

以上为完整的 UI 设计方案，涵盖所有页面的线框、交互、组件结构和视觉规范。
确认后即可进入 Phase 1 代码实现阶段。
