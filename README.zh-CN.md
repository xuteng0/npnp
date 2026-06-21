# npnp

<p align="center">
  <img src="assets/npnp.png" alt="npnp app logo" width="360">
</p>

<p align="center">
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/version-1.0.2-e05d44?style=flat-square" alt="version 1.0.2"></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-Apache--2.0-f08c5a?style=flat-square" alt="license Apache 2.0"></a>
  <a href=".github/workflows/windows-release.yml"><img src="https://img.shields.io/badge/platform-Windows-2ea44f?style=flat-square" alt="platform Windows"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-edition%202024-f28d1a?style=flat-square" alt="rust edition 2024"></a>
</p>

<p align="center">
  <a href="README.md">English</a> | 简体中文
</p>

Normalize Pin Net Pad (`npnp`) 是一个用纯 Rust 编写的 LCEDA/EasyEDA 下载器和 Altium 库导出工具。

`npnp` 可以搜索 LCEDA/LCSC 器件，下载上游 EasyEDA 源数据和 3D 模型，并导出 Altium 兼容的原理图库和 PCB 封装库。

## 项目状态

### 已实现

- [x] 按关键字、器件名称或 LCSC 编号搜索 LCEDA/LCSC 器件。
- [x] 下载 STEP 或 OBJ/MTL 格式的 3D 模型。
- [x] 导出 EasyEDA 原理图符号和 PCB 封装源 JSON，便于检查。
- [x] 导出 Altium 原理图库 (`.SchLib`)。
- [x] 导出 Altium PCB 封装库 (`.PcbLib`)。
- [x] 在上游提供 STEP 模型时，将 STEP 嵌入 PCB 封装库。
- [x] 从文本文件批量导出多个 LCSC 编号。
- [x] 支持单个器件单独导出，也支持合并为库文件对。
- [x] 支持向已有合并库追加新器件，并避免重复添加已有 LCSC 编号。
- [x] 支持通过 `--lcsc-english` 导出可选的英文 LCSC 元数据。

### Roadmap

- [ ] 尽可能移除下载 3D 模型中的 logo/watermark 几何体。
- [ ] 改进不规则焊盘导出 `.PcbLib` 时的阻焊处理。
- [ ] 为更多异常 EasyEDA 符号和封装增加回归测试样例。
- [ ] 完善批量合并和追加工作流文档。

### 已知限制

- 生成的库文件在用于生产前仍然应该目检确认。
- 部分上游 EasyEDA 符号和封装可能使用需要特殊处理的图元。

原理图库截图：

<p align="center">
  <img src="imgs/sch_01.png" alt="Merged schematic screenshot 1" width="32%">
  <img src="imgs/sch_02.png" alt="Merged schematic screenshot 2" width="32%">
  <img src="imgs/sch_03.png" alt="Merged schematic screenshot 3" width="32%">
  <img src="imgs/sch_04.png" alt="Merged schematic screenshot 4" width="32%">
  <img src="imgs/sch_05.png" alt="Merged schematic screenshot 5" width="32%">
  <img src="imgs/sch_06.png" alt="Merged schematic screenshot 6" width="32%">
</p>

PCB 封装库截图：

<p align="center">
  <img src="imgs/pcb_01.png" alt="Merged PCB screenshot 1" width="32%">
  <img src="imgs/pcb_02.png" alt="Merged PCB screenshot 2" width="32%">
  <img src="imgs/pcb_03.png" alt="Merged PCB screenshot 3" width="32%">
  <img src="imgs/pcb_04.png" alt="Merged PCB screenshot 4" width="32%">
  <img src="imgs/pcb_05.png" alt="Merged PCB screenshot 5" width="32%">
  <img src="imgs/pcb_06.png" alt="Merged PCB screenshot 6" width="32%">
</p>

## Pull requests

欢迎提交 Pull Request。

- Issue 和 Pull Request 均欢迎使用中文或英文。
- 请尽量保持改动聚焦，并说明 PR 解决的问题。
- 如果修改导出器，建议附上用于测试的 LCSC 编号或 fixture。
- 如果改动会影响生成的 `.SchLib` 或 `.PcbLib`，请在提交前目检生成结果。
- 较大的改动建议先开 issue 讨论范围。

## 如何使用 CLI 工具

在终端中直接输入 `npnp`，然后复制提示中的命令即可。

通常最常用的是最后两条批量命令：它们可以批量导出 `schlib` 和 `pcblib`，也可以向 `npnp` 已生成的合并库继续追加新器件。

```bash
~ -> npnp
Normalize Pin Net Pad (npnp) - Pure Rust LCEDA downloader and bundle exporter

Usage: npnp [OPTIONS] [COMMAND]

Commands:
  search         Search components by keyword
  download-step  Search by keyword and download STEP by result index
  download-obj   Search by keyword and download OBJ/MTL by result index
  export-source  Export EasyEDA symbol / footprint JSON sources only
  export-schlib  Export a pure Rust Altium schematic library (.SchLib)
  export-pcblib  Export a pure Rust Altium PCB footprint library (.PcbLib)
  bundle         Export a pure-Rust input bundle: sources + STEP + manifest
  batch          Batch export Altium libraries from a text file of LCSC IDs
  help           Print this message or the help of the given subcommand(s)

Options:
      --prompt   Show ready-to-run example commands
  -h, --help     Print help
  -V, --version  Print version
```

```bash
~ -> npnp --prompt
Normalize Pin Net Pad (npnp) ready-to-run commands:

Search a component
  npnp search C2040 --limit 5

Export one schematic library
  npnp export-schlib C2040 --index 1 --output schlib --force

Export one PCB library
  npnp export-pcblib C2040 --index 1 --output pcblib --force

Export EasyEDA source JSON plus STEP bundle
  npnp bundle C2040 --index 1 --output bundle --force

Batch export both libraries from ids.txt
  npnp batch --input ids.txt --output generated\quick_check --full --force --continue-on-error

Merge both libraries into one pair of outputs
  npnp batch --input ids.txt --output generated\merged --merge --library-name MyLib --full --continue-on-error

Append new parts into an existing merged library
  npnp batch --input new_ids.txt --output generated\merged --merge --append --library-name MyLib --full --continue-on-error
```

## 如何使用 GUI 工具

另一种更简单的方式是使用 [`SeEx`](https://github.com/linkyourbin/seex)，强烈推荐。

如果你不知道如何使用 `SeEx`，我也做了一个 bilibili 演示视频：[【工具分享】你应该把时间花费在电路设计和 Layout 上，而不是机械重复的绘制原理图和封装](https://www.bilibili.com/video/BV1bEEE6mEHd)
