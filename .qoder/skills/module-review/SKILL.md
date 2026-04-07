---
name: module-review
description: 深度审查项目模块，分析模块在系统架构中的定位、关键作用、当前实现及潜在问题。当用户需要对某个模块进行架构审查、代码评审或理解模块职责时使用。
tools: Read, Glob, Grep, Agent
---

# 模块 Review Skill

对 KeyCompute 项目的指定模块进行深度架构分析和代码审查，帮助理解模块职责、发现潜在问题、提出改进建议。

## 使用方式

```
/module-review <模块名或路径>
```

示例：
- `/module-review keycompute-routing`
- `/module-review crates/keycompute-auth`
- `/module-review llm-gateway`

---

## Phase 1: 模块定位分析

**目标**: 确定模块在系统架构中的位置和边界

**操作**:
1. 读取模块的 `lib.rs` 或 `main.rs` 入口文件
2. 分析模块的 Cargo.toml 依赖关系（上游/下游）
3. 追踪模块在调用链中的位置：
   - 谁调用了这个模块？（调用者）
   - 这个模块调用了谁？（被调用者）
4. 绘制模块在系统架构中的位置图

**输出**:
```
模块定位：
- 层级：[基础设施层 / 领域层 / 应用层 / 接口层]
- 上游依赖：[列出依赖模块]
- 下游被依赖：[列出被哪些模块依赖]
- 外部依赖：[数据库、Redis、外部API等]
```

---

## Phase 2: 关键职责分析

**目标**: 理解模块的核心职责和对外接口

**操作**:
1. 提取模块公开的公共 API（pub fn, pub struct, pub trait）
2. 分析核心数据结构和领域模型
3. 识别模块的主要业务流程
4. 确定模块的输入输出边界

**输出**:
```
关键职责：
- 核心职责：[用一句话概括]
- 公开接口：[列出主要 pub API]
- 核心数据结构：[主要 struct/enum]
- 业务流程：[主要处理流程]
```

---

## Phase 3: 实现深度分析

**目标**: 深入理解模块的内部实现

**操作**:
1. 读取模块的核心实现文件
2. 分析代码组织结构（子模块划分）
3. 识别关键算法和业务逻辑
4. 检查错误处理模式
5. 分析测试覆盖情况

**检查点**:
- [ ] 代码组织是否清晰
- [ ] 错误处理是否完善
- [ ] 是否有充分的单元测试
- [ ] 是否有集成测试
- [ ] 是否有文档注释
- [ ] 是否有性能考虑

---

## Phase 4: 矛盾冲突检测

**目标**: 发现模块设计中的潜在问题

**检测维度**:

### 4.1 架构冲突
- 职责边界是否清晰（是否违反单一职责）
- 是否存在循环依赖
- 层级是否正确（是否跨层调用）
- 抽象层次是否一致

### 4.2 数据一致性冲突
- 状态管理是否合理
- 并发访问是否安全
- 数据流是否清晰
- 缓存一致性是否有保障

### 4.3 接口契约冲突
- 接口是否稳定
- 错误返回是否一致
- 版本兼容性是否有考虑
- 是否有 Breaking Change 风险

### 4.4 性能与可扩展性冲突
- 是否有性能瓶颈
- 扩展性设计是否合理
- 资源管理是否得当
- 是否有过度设计

### 4.5 安全性冲突
- 输入验证是否充分
- 权限控制是否到位
- 敏感数据是否安全处理
- 是否有注入风险

---

## Phase 5: 改进建议

**目标**: 提供具体的改进方向

**输出格式**:
```markdown
## Review 报告：[模块名]

### 1. 模块定位
[架构位置图和依赖关系]

### 2. 关键职责
[核心职责和公开接口摘要]

### 3. 实现评估
| 维度 | 评分 | 说明 |
|------|------|------|
| 代码组织 | ⭐⭐⭐⭐☆ | ... |
| 错误处理 | ⭐⭐⭐☆☆ | ... |
| 测试覆盖 | ⭐⭐☆☆☆ | ... |
| 文档完整 | ⭐⭐⭐☆☆ | ... |
| 性能考虑 | ⭐⭐⭐⭐☆ | ... |

### 4. 发现的问题

#### 🔴 高优先级
- [问题描述]
- [问题描述]

#### 🟡 中优先级
- [问题描述]

#### 🟢 低优先级
- [问题描述]

### 5. 改进建议
1. [具体建议]
2. [具体建议]
3. [具体建议]

### 6. 架构图
[ASCII 或 Mermaid 架构图]
```

---

## 工作流程总结

```
模块名/路径
    │
    ▼
┌─────────────────┐
│  Phase 1: 定位  │ ─→ 分析依赖关系，确定架构位置
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Phase 2: 职责  │ ─→ 提取公共API，理解核心职责
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Phase 3: 实现  │ ─→ 深入代码，评估实现质量
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Phase 4: 冲突  │ ─→ 检测架构/数据/接口/性能问题
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Phase 5: 报告  │ ─→ 输出完整 Review 报告
└─────────────────┘
```

---

## 项目特定知识

### KeyCompute 模块清单

| 模块 | 路径 | 职责领域 |
|------|------|---------|
| keycompute-server | crates/keycompute-server | HTTP API 网关层 |
| keycompute-types | crates/keycompute-types | 共享类型定义 |
| keycompute-config | crates/keycompute-config | 配置管理 |
| keycompute-db | crates/keycompute-db | 数据库访问层 |
| keycompute-auth | crates/keycompute-auth | 认证鉴权 |
| keycompute-ratelimit | crates/keycompute-ratelimit | 分布式限流 |
| keycompute-pricing | crates/keycompute-pricing | 定价引擎 |
| keycompute-routing | crates/keycompute-routing | 智能路由引擎 |
| keycompute-runtime | crates/keycompute-runtime | 运行时状态 |
| keycompute-billing | crates/keycompute-billing | 计费结算 |
| keycompute-distribution | crates/keycompute-distribution | 二级分销 |
| keycompute-observability | crates/keycompute-observability | 可观测性 |
| keycompute-emailserver | crates/keycompute-emailserver | 邮件服务 |
| llm-gateway | crates/llm-gateway | LLM 执行网关 |
| llm-provider | crates/llm-provider | Provider 适配器 |
| keycompute-payment | crates/keycompute-payment | 支付模块 |

### 关键架构约束

1. **分层原则**: 上游模块不应依赖下游模块
2. **Provider 抽象**: 所有 LLM Provider 通过统一 trait 抽象
3. **异步优先**: 所有 I/O 操作使用 async/await
4. **错误传播**: 使用统一的 Error 类型体系

---

## 执行指南

当用户调用此 skill 时：

1. **确认模块**: 如果参数不明确，使用 AskUserQuestion 确认要分析的模块
2. **按顺序执行**: 严格按 Phase 1-5 顺序执行
3. **深度优先**: 每个阶段都要深入分析，不要浅尝辄止
4. **具体举例**: 在报告中提供具体的代码示例说明问题
5. **可操作建议**: 改进建议要具体、可执行

开始执行时，首先使用 Glob 和 Grep 工具定位模块文件，然后使用 Read 深入读取代码。