# Codex 渲染代码来源

来源：https://github.com/openai/codex

固定提交：`f1affbac5e5164b2bae825e9b39e9868bc4e0be2`。

`src/agent/chat/render/` 移植该提交的 Markdown、表格、折行、链接文本与行转换辅助代码。修改模块路径、裁剪未使用接口；高亮适配为固定主题，不引入 Codex 配置、运行时或终端 OSC 8 写入。工具展示由 Twig 根据自身消息协议实现。

上游许可证与归属分别见 [LICENSE](LICENSE)、[NOTICE](NOTICE)。后续更新须核对本地适配与渲染测试。
