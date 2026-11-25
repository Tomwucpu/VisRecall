# 目记 VisRecall

VisRecall 是 Windows 上的本地素材库，用于收集截图、图片和文本，并通过中文 OCR、全文和语义检索找回素材。

## 安装

1. 在 [GitHub Releases](https://github.com/Tomwucpu/VisRecall/releases) 选择要安装的版本，按目标机器能否联网选择安装包。两者都随包携带本地模型和 ONNX Runtime，安装后都能断网使用本地 OCR 与语义检索：

   | 安装包 | 体积 | 选择场景 |
   | --- | --- | --- |
   | `VisRecall_<版本>_x64-setup.exe` | 约 42 MB | 机器可以联网。系统缺少 WebView2 时，安装程序会自动下载并安装运行时；Windows 11 和 Windows 10（1803 及以后）通常已自带。 |
   | `VisRecall_<版本>_x64-setup-offline.exe` | 约 330 MB | 机器不能联网，或安装程序被策略限制联网。安装包内置 WebView2 Evergreen 独立安装程序，无需下载。 |

2. 下载同版本页面上的 `SHA256SUMS.txt`，在 PowerShell 核对所选安装包，例如 `Get-FileHash -Algorithm SHA256 -LiteralPath .\VisRecall_0.1.0_x64-setup.exe`，结果应与校验文件中对应行一致。
3. 为本地 OCR 和语义检索准备 [Microsoft Visual C++ v14 Redistributable（x64）](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)。离线机器可先下载该运行库安装程序。两种安装包都会安装 WebView2 运行时，差别只在于 WebView2 是否随包提供。
4. 运行安装包，先选「安装位置」，存放程序和随包模型。首次安装再选择「素材库位置」，默认是 `%USERPROFILE%\Documents\VisRecall`。也可选择可写的新目录或空目录，例如 `D:\VisRecall 数据\素材库`。两处目录需要分开。
5. 从开始菜单启动 VisRecall。应用默认在系统托盘启动，点击托盘图标打开主窗口。在「设置 → 存储」可以核对素材库路径，之后也可在该处迁移素材库。

首发候选包面向 Windows 11 x64。实际验收通过的 Windows 版本以对应 Release 的说明为准。未签名候选包可能触发 Windows SmartScreen 警告或被 Smart App Control 阻止，签名状态也以该 Release 的说明为准。

## 使用

- 截图默认关闭。在「设置 → 捕获」开启截图后，可使用设置中的全局热键或悬浮球截图。
- 可以导入图片、文件夹和文本。截图和复制入库的素材存放在素材库目录，引用导入的原始文件仍留在原处。
- VisRecall 的云端解析默认关闭。本地 OCR、全文检索和语义检索可在断网时使用。
- 关闭主窗口后，应用继续驻留系统托盘。彻底退出时，右键点击托盘图标并选择「退出」。
- 同版重装、升级和卸载保留素材库和应用配置。卸载后重装可以继续使用原素材库。

## 反馈与第三方声明

在 [GitHub Issues](https://github.com/Tomwucpu/VisRecall/issues) 反馈问题时，请提供 VisRecall 版本、Windows 版本、安装包 SHA-256、复现步骤和错误信息。请先移除截图、库文件、凭据和日志中的个人信息。

[第三方声明](THIRD_PARTY_NOTICES.txt)记录随包模型和 ONNX Runtime 的来源及许可。安装器模板的许可随安装包提供。

## 许可证

VisRecall 以 [Apache License 2.0](LICENSE) 发布，版权归 Tomwu 所有。可以自由使用、修改、分发和商用，需保留版权与许可声明、在修改过的文件中标注改动，并在分发时附带上文列出的第三方声明。该许可同时提供明确的专利授权。
