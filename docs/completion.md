# Shell 补全

命令行阅读：`twig guide completion`。

安装或更新 twig 后，在 `~/.zshrc` 中加入：

```zsh
autoload -Uz compinit
compinit
source <(COMPLETE=zsh twig)
```

如果 Oh My Zsh 等配置已经运行 `compinit`，只需在它之后加入最后一行。重新打开终端，或在当前 Zsh 中执行上述配置即可启用。每次启动 Shell 都重新生成补全接入脚本，升级 twig 后重新打开终端即可。

支持命令、选项及以下动态值：

| 输入后按 Tab | 候选来源 |
| --- | --- |
| `twig ls ` / `new ` / `open ` / `stop ` / `rm ` | 已登记项目别名 |
| `twig open myapp ` / `new myapp ` / `stop myapp ` / `rm myapp ` | 该项目的已有任务；`new` 仍可输入新名称 |
| `twig new myapp auth --base `、`twig rm myapp auth --target ` | 该项目已有本地分支，不含远端跟踪分支 |
| `twig add myapp --path /repo --base ` / `--target ` | 指定路径的已有本地分支 |
| `--config `、`--path `、`--root ` | 文件或目录路径 |

动态候选按输入前缀过滤，识别光标前的项目别名和 `--config PATH`（也支持 `--config=PATH`）。从任意目录操作同一别名，任务候选相同。

带空格的配置路径可使用单引号、双引号或反斜杠转义。目录补全支持空格、冒号及 `--root=PATH` 写法，保留末尾 `/`，可以继续输入并补全子目录。

`twig open myapp <Tab>`（以及 `new`、`stop`、`rm`）先展示任务名，再展示带说明的选项；Tab 选择也优先任务名。任务按名称字典序排列。输入 `twig open myapp a<Tab>` 只匹配该前缀的任务，输入 `twig open myapp --<Tab>` 只显示选项。升级后在当前 Zsh 重新执行 `source <(COMPLETE=zsh twig)` 即可加载新的展示规则。

补全只读本地数据，不联网、不创建配置或锁、不调用 tmux，也不执行正在输入的任务命令。未初始化、未登记或路径缺失的项目可能没有相应候选；配置损坏时不会修改或覆盖文件。

指南主题名也参与静态补全。当前使用说明和交互验证范围为 Zsh。
