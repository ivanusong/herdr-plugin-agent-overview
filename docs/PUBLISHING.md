# 发布到 GitHub（PUBLISHING）

本文说明把本插件作为公开 Herdr 插件发布时的要求与建议流程。所有条目都可
在 GitHub 仓库文件或仓库设置中直接落实，无需修改代码。

## 仓库要求

- **默认分支根部包含 `herdr-plugin.toml`**：Herdr marketplace 只索引默认分支
  上根目录或子目录中的 `herdr-plugin.toml`；本仓库放在根目录。
- **manifest 校验**：`id`、`name`、`version`、`min_herdr_version` 为必填；
  `platforms = ["macos", "linux"]` 已声明；`min_herdr_version = "0.9.4"` 为
  已验证的最低版本。
- **License**：MIT（见 [LICENSE](../LICENSE)，含上游 herdr-mission-control 的
  MIT 版权声明，发布时请保留）。
- **建议 repo 名**：`herdr-plugin-agent-overview`（与文档主标题一致；
  marketplace 按 manifest 展示，不强制仓库名，但一致更利于检索）。
- **插件 ID**：`ivan.agent-overview`；action 为 `ivan.agent-overview.open`。
- **本仓库目录结构已符合**：`Cargo.toml`/`Cargo.lock`、`src/`、`herdr-plugin.toml`、
  `scripts/build.sh`、`LICENSE`、`README.md`、`.github/workflows/ci.yml`。

## GitHub 仓库设置

- **Topic**：`herdr-plugin`（marketplace 自动索引的唯一发现标记，索引每 30 分钟
  刷新一次）。可按需添加 `herdr`、`terminal`、`tui`、`ratatui` 等辅助 topic。
- **描述**：建议与 manifest `description` 一致（英文更利于国际用户），例如
  `Full-screen overview of working agents for Herdr (Rust/ratatui popup)`。
- **Website**：可留空或指向 README 中的说明。
- **CI**：仓库自带 `.github/workflows/ci.yml`，在 `ubuntu-latest` 与
  `macos-latest` 上运行 `cargo fmt --check`、`cargo clippy --all-targets
  --locked -- -D warnings`、`cargo test --locked`、`cargo build --release
  --locked`。建议在分支保护中要求该检查通过。

## 发布流程（可选）

1. 确保 `main` 上 CI 通过、`herdr-plugin.toml` 的 `version` 已按语义化版本递增。
2. 打 tag 并在 GitHub 创建 Release（例如 `v0.1.0`），Release 说明写用户可见
   变更；tag 可附加源码 zip（GitHub 自动提供）。
3. GitHub 安装的用户无需关心构建产物：`herdr plugin install <owner>/<repo>`
   会执行 manifest 的 `[[build]]`（即 `sh scripts/build.sh`），在用户机器上
   用 Rust 工具链本地编译。
4. 更新/重装：`herdr plugin update <owner>/<repo>`；已运行的命令/面板不受影响。

## 校验和（SHA-256）说明

本插件当前不发布预编译二进制，因此 Manifest 与 README 不涉及二进制校验和；
`herdr plugin install/update` 直接拉取源码并本地构建。若未来改为发布预编译
二进制，请在 Release 附件旁提供各目标二进制的 SHA-256 校验值（例如
`sha256sum herdr-agent-overview-<target> > herdr-agent-overview-<target>.sha256`），
并让安装文档说明校验方式；这属于发行渠道变更，需要同步更新 README 与
manifest 的 `[[build]]` 流程。

## 本地验收命令（发布前）

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
sh scripts/build.sh
herdr plugin link "$(pwd)"          # 本地 link 验收（不改全局配置）
herdr plugin action invoke ivan.agent-overview.open
```
