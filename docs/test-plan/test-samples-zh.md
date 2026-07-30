# Myna Test Reading Sample Corpus (Mandarin Chinese / 中文)

Mandarin Chinese (`zh`) translation of the reading sample corpus used by
`docs/test-plan-system.md`. Mirrors the structure of `docs/test-samples-en.md`
§1–§6.

> **Review status**: draft machine-assisted translation — **needs
> native/fluent Mandarin speaker review before use**, per
> `docs/test-plan-system.md` §2's requirement that accuracy judgments be
> made only by a fluent/native speaker of the language being tested. This
> draft is written in Simplified Chinese; confirm whether Traditional
> Chinese should be preferred for any particular tester.

**Product-specific terms convention**: product names, package names, and
technical identifiers (myna, whisper-snap, nemotron-snap, PipeWire, IBus,
Nemotron, GNOME Shell, version strings, file paths) are kept in **English**
throughout, matching how a real bilingual user would actually speak them —
these are not translated.

---

## 1. Natural long-form prose (2 passages)

**Passage A** (~35 seconds spoken)

> "人工智能研究一直专注于几个关键目标：推理、知识表示、规划、学习、自然语言
> 处理和感知。通用智能，即完成人类能够胜任的任何任务的能力，是该领域的长期
> 目标之一。为了实现这些目标，研究人员使用了广泛的技术，包括搜索和数学优化、
> 形式逻辑、人工神经网络，以及基于统计学、概率论和经济学的方法。"

**Passage B** (~30 seconds spoken)

> "每一个使用电脑和网络的组织都面临一套基本的网络安全风险。员工可以通过使用
> 强而独特的密码、及时更新软件，以及对来自未知发件人的附件和链接保持警惕，
> 来帮助管理这些风险。多因素身份验证即使在密码被盗的情况下，也能增加一层额外
> 的保护。定期备份重要文件意味着勒索软件攻击或硬件故障不一定会导致数据永久
> 丢失。"

## 2. Command / short-utterance set

1. "打开一个新的终端窗口。"
2. "把这个在周五之前发给团队。"
3. "逗号，新段落，句号。"
4. "撤销这个操作。"
5. "安排明天下午三点的会议。"
6. "回复：听起来不错，到时候见。"
7. "搜索附近的咖啡店。"
8. "静音麦克风。"
9. "换行。谢谢，回头聊。"
10. "取消吧，算了。"

## 3. Domain / technical vocabulary passage

> "我安装了 myna-desktop snap，同时也装了 whisper-snap 和 nemotron-snap，
> 然后确认 PipeWire 正确地路由了我的麦克风。快捷键会触发 IBus 注入，我还
> 启用了 preedit 选项，可以在文本提交之前预览不稳定的文本。升级到 version
> one point three point zero 之后，我检查了 tilde slash dot config slash
> myna slash settings dot json 里的配置，确认 streaming 模式仍然设置为
> auto。GNOME Shell 扩展会显示活动指示器，而不会把焦点从我的终端上抢走。"

## 4. Numbers, dates, and punctuation-heavy passage

> "请拨打五五五，零一四二联系我，日期是二零二六年七月二十九号。发票总额是
> 四百一十二美元五十美分，需在三十天内付清。我的航班早上六点四十五分从
> B十二号登机口起飞，确认码是 X-Ray Tango 四七一。"

## 5. Pangram / phonetic smoke-test

> "视频可为提供请求服务的用户提供最新资讯，帮助确保区域性医联体获得更多产品。"

*(A commonly used Mandarin phonetically-dense sentence, chosen for broad
initial/final coverage rather than as a literal translation of the English
fox pangram — Mandarin does not have a directly equivalent single
standardized pangram tradition.)*

## 6. Long continuous passage for streaming tests (30s+)

> "研究人员本周宣布，一颗新的气象卫星已开始从轨道传输数据，为气象学家提供
> 比以往几代仪器更高分辨率的图像。这颗卫星于今年早些时候发射，携带的传感器
> 能够近乎实时地追踪风暴系统，官员表示这应该能改善沿海社区的早期预警。与此
> 同时，任务地面控制中心的工程师确认，所有机载系统都在预期参数范围内运行，
> 航天器已成功完成首次轨道调整机动。下一个重大里程碑，即成像仪器的全面校准，
> 预计将在未来一个月内完成，之后该卫星将开始其常规运行服务。"
