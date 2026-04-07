# Agent-Task 快速关联 UX 改进

## Context
当前 agent 和监控任务的关联只能从任务详情页 (`/tasks/:taskUuid`) 逐个操作，体验差：
- Agent 详情页看不到已分配任务，无法从 agent 侧操作
- 创建任务/agent 时无法同时关联
- 任务列表看不到已分配 agent 数量
- API 支持批量分配 (`agent_uuids: string[]`)，但前端只用了单个
- `GET /agents/{agent_uuid}/tasks` 端点存在但前端未使用

目标：全面提升关联效率 — 双向关联、创建时关联、列表页快捷操作、多选。

## 文件清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/api/hooks/keys.ts` | 修改 | 新增 `agentKeys.tasks(uuid)` |
| `src/api/hooks/use-agent-tasks.ts` | **新建** | `useAgentTasks` + 双向 assign/unassign hooks |
| `src/api/hooks/use-task-assignments.ts` | 修改 | 添加 `agentKeys.all` 交叉缓存失效 |
| `src/components/ui/checkable-list.tsx` | **新建** | 可复用的多选切换列表组件 |
| `src/features/agents/pages/agent-detail-page.tsx` | 修改 | 新增「已分配任务」面板 |
| `src/features/tasks/pages/task-detail-page.tsx` | 修改 | 单选改多选切换 |
| `src/features/tasks/pages/tasks-page.tsx` | 修改 | 创建时选 agent + 列表页管理 agent 按钮 |
| `src/features/tasks/components/assign-agents-dialog.tsx` | **新建** | 快速分配 agent 弹窗 |
| `src/features/agents/pages/agents-page.tsx` | 修改 | 创建时选任务 |
| `src/i18n/locales/en.json` | 修改 | ~10 个新 key |
| `src/i18n/locales/zh.json` | 修改 | 对应中文 |

## 实现步骤

### 1. 数据层：Query Key + Hooks

**`keys.ts`** — `agentKeys` 新增：
```ts
tasks: (agentUuid: string) => [...agentKeys.all, 'tasks', agentUuid] as const,
```

**`use-agent-tasks.ts`**（新建 ~65 行）— 3 个 hook：
- `useAgentTasks(agentUuid)` — 包装 `getAgentTasksApiV1AgentsAgentUuidTasksGet`，key 为 `agentKeys.tasks(agentUuid)`
- `useAssignTasksFromAgent()` — 调用 `assignTaskEndpointApiV1TasksTaskUuidAssignPost`，成功后同时失效 `taskKeys.agents` + `agentKeys.tasks`
- `useUnassignTaskFromAgent()` — 调用 `unassignTaskEndpointApiV1TasksTaskUuidAgentsAgentUuidDelete`，同上

**`use-task-assignments.ts`** — `useAssignAgents` 和 `useUnassignAgent` 的 `onSuccess` 新增：
```ts
queryClient.invalidateQueries({ queryKey: agentKeys.all })
```

### 2. 可复用组件：CheckableList

**`src/components/ui/checkable-list.tsx`**（新建 ~55 行）

```ts
interface CheckableListItem {
  id: string
  label: string
  sublabel?: string
  disabled?: boolean
}
interface CheckableListProps {
  items: readonly CheckableListItem[]
  selectedIds: ReadonlySet<string>
  onToggle: (id: string) => void
  emptyMessage?: string
}
```

- 每行可点击，选中显示 ✓ + 绿色边框，未选中无标记
- `disabled` 行半透明不可点击（用于 in-flight 保护）
- 空列表显示 `emptyMessage`
- `role="listbox" aria-multiselectable="true"`，每行 `role="option" aria-selected`

### 3. Agent 详情页：已分配任务面板

**`agent-detail-page.tsx`** — 在现有 2 列 grid 下方新增全宽面板（+~80 行）

新增 hooks：`useAgentTasks`, `useTasks`, `useAssignTasksFromAgent`, `useUnassignTaskFromAgent`, `useAuthStore`

面板内容：
- 标题：`t('agents.assignedTasks')`
- Admin：Select 下拉选择可用任务 + 分配按钮（复用 task-detail-page 的 Select 模式）
- 任务列表：表格显示 Name / Protocol Badge / Target / Actions(Remove)
- 空态：`t('agents.noTasksAssigned')`

分配逻辑：选择任务后调用 `assignTasks.mutate({ taskUuid, agentUuid, data: { agent_uuids: [agentUuid] } })`

### 4. 任务详情页：单选改多选

**`task-detail-page.tsx`** — 替换 Select+Assign 为 CheckableList（净减 ~5 行）

- 删除 `selectedAgentUuid` state 和 `handleAssign`
- 新增 `pendingIds: Set<string>` state 追踪 in-flight
- CheckableList 显示所有非 disabled agent，已分配的预选中
- 点击已选 → unassign，点击未选 → assign
- `pendingIds` 防止重复点击，`onSettled` 清理

### 5. 创建任务时选 Agent

**`tasks-page.tsx`** — 创建对话框新增可选 agent 列表（+~30 行）

- 新增 `selectedAgentUuids: Set<string>` state
- 新增 `useAgents()` + `useAssignAgents()` hooks
- 表单底部加 CheckableList（Label: `t('tasks.assignAgentsOptional')`）
- `handleCreate` 的 `onSuccess` 中：如果有选中 agent，调用 `assignAgents.mutate({ taskUuid: result.task_uuid, data: { agent_uuids: [...selectedAgentUuids] } })`

### 6. 创建 Agent 时选任务

**`agents-page.tsx`** — 创建对话框新增可选任务列表（+~35 行）

- 新增 `selectedTaskUuids: Set<string>` state
- 新增 `useTasks()` + `useAssignAgents()` hooks
- 表单底部加 CheckableList（Label: `t('agents.assignTasksOptional')`）
- `handleCreate` 的 `onSuccess` 中：对每个选中的 taskUuid 调用 assign（`Promise.allSettled` 处理部分失败）
- 扩展 response cast 包含 `agent_uuid`

### 7. 任务列表页：管理 Agent 快捷操作

**`tasks-page.tsx`** — Admin 表格新增「管理探针」按钮（+~15 行）

- 新增 `assignDialogTaskUuid: string | null` state
- Admin 行新增按钮：`t('tasks.manageAgents')`，点击设置 `assignDialogTaskUuid`

**`assign-agents-dialog.tsx`**（新建 ~55 行）

- Props: `taskUuid: string | null`, `onClose: () => void`
- 内部调用 `useTaskAgents` + `useAgents` + `useAssignAgents` + `useUnassignAgent`
- Dialog 内渲染 CheckableList，已分配 agent 预选中
- 切换逻辑同步骤 4，带 `pendingIds` 保护
- Footer: Done 按钮关闭

### 8. i18n

**en.json 新增：**
| Key | Value |
|-----|-------|
| `tasks.assignAgentsOptional` | `"Assign Agents (optional)"` |
| `tasks.manageAgents` | `"Manage Agents"` |
| `tasks.manageAgentsDesc` | `"Toggle agents assigned to this task."` |
| `agents.assignedTasks` | `"Assigned Tasks"` |
| `agents.noTasksAssigned` | `"No tasks assigned to this agent."` |
| `agents.selectTask` | `"Select a task..."` |
| `agents.noAvailableTasks` | `"No available tasks"` |
| `agents.assignTasksOptional` | `"Assign Tasks (optional)"` |

**zh.json 对应中文翻译。**

## 验证

1. `npx tsc -b --force` 零错误
2. `npm run build` 成功
3. Agent 详情页：显示已分配任务列表，可分配/移除任务
4. 任务详情页：点击 agent 行即可切换分配状态（多选）
5. 创建任务：选择 agent → 创建后自动分配
6. 创建 Agent：选择任务 → 创建后自动分配
7. 任务列表：点击「管理探针」→ 弹窗切换 agent 分配
8. 双向缓存：从 agent 侧分配后，任务侧数据自动刷新，反之亦然
9. 中英文切换：所有新文案正确显示
