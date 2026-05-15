# sem (Rust)

语义化版本控制 CLI。基于 Git 的实体级 diff、blame、依赖图和影响分析。

Git 告诉你 *第 43 行改了*。Sem 告诉你 *src/auth.ts 里的 validateToken 函数被修改了*。

## 为什么需要 sem

Git 的行级模型和开发者的思维方式不匹配。你不关心第 12-18 行变了，你关心的是 `validateToken` 被修改了、`legacyAuth` 被删除了。当 AI Agent 在改代码时，这一点更加重要——Agent 需要理解 *改了什么*，而不是 *文件哪个位置变了*。

## 命令

```bash
# 实体级 diff
sem diff
sem diff --file src/auth.ts              # 只看某个文件的变化
sem diff HEAD~3..HEAD                     # 比较两个 ref
sem diff --staged                         # 只看暂存区的变化

# 实体级 blame（谁最后修改了每个函数/类）
sem blame src/auth.ts

# 列出文件或目录中的实体
sem entities src/auth.ts

# 跨文件依赖图
sem graph

# 影响分析（如果修改了这个实体，还有哪些会受影响？）
sem impact validateToken

# 实体历史 — 追踪一个函数在提交历史中的演变
sem log validateToken
sem log "Process(int,string)"             # 重载感知：指定签名
sem log validateToken --file src/auth.ts  # 实体在多个文件中存在时，用 --file 消歧
sem log validateToken -v                  # 显示内联内容 diff

# 实体的上下文窗口（token 预算，适合 AI Agent 使用）
sem context validateToken

# 跨代码库验证函数调用参数数量
sem verify

# 按语言过滤（适用于多语言仓库）
sem graph --file-exts .py
sem diff --file-exts .py .rs
sem impact validateToken --file-exts .py
```

## 重载支持

所有方法都会显示参数签名（无参数时显示 `()`）。当方法有多个重载时，通过签名进行消歧：

```bash
# 列出实体 — 每个方法都显示签名
sem entities src/auth.ts
#   method validateToken() (L10:25)
#   method validateToken(string) (L30:45)

# 通过签名追踪某个重载的历史
sem log "validateToken(string)"

# 无参数重载 — 使用空括号
sem log "validateToken()"

# 如果不指定签名且存在重载，sem 会列出所有重载让你选择
sem log validateToken
#   error: Entity 'validateToken' has 2 overloads:
#     method validateToken() (L10:25)
#     method validateToken(string) (L30:45)
#   Specify the signature to disambiguate: sem log "validateToken()"
```

`sem log` 自动追踪重命名、签名变更和跨文件移动。

## 支持的语言

23 个 tree-sitter 解析插件：TypeScript、JavaScript、Python、Go、Rust、Java、C、C++、C#、Ruby、PHP、Swift、Elixir、Bash、HCL/Terraform、Kotlin、Fortran、Perl、Dart、OCaml，以及 Vue、Svelte、ERB。支持通过 shebang 检测无扩展名文件。

不支持的语言会回退到基于块的 diff。

## 架构

Cargo workspace，包含两个 crate：

```
sem-core/    # 库：实体提取、结构化哈希、语义 diff、依赖图、影响分析、git 桥接
sem-cli/     # 二进制：diff、blame、graph、impact、log、entities、context、verify 命令
```

### sem-core

weave、agenthub、effect-system、agent-lint、unified-build 和 agent-bench 都依赖此库。

- **解析器注册表** — 23 个语言插件，基于 tree-sitter + shebang 检测
- **结构化哈希** — AST 归一化，忽略空白和注释
- **语义 diff** — 三阶段实体匹配（精确 ID → 内容哈希 → 模糊相似度）
- **重载感知实体追踪** — 签名匹配和消歧
- **表面修改 vs 结构修改** 检测
- **实体依赖图** — 跨文件调用/引用关系
- **影响分析** — 通过依赖图的传递 BFS
- **实体历史追踪** — 跨重命名、签名变更和文件移动
- **Git 桥接** — 在任意 ref 读取文件内容

## 构建

```bash
cargo build --release
# 二进制文件在 target/release/sem
```

## 测试

```bash
cargo test
# 283 个测试
```

## 许可证

MIT
