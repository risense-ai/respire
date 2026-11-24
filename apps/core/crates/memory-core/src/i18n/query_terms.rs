pub(crate) const HOW_TO_TERMS: [&str; 26] = [
    "怎么", "如何", "怎样", "咋办", "报错", "失败", "错误", "步骤", "流程", "命令", "脚本",
    "配置", "安装", "部署", "调试", "排查", "修复", "跑通", "操作", "上传", "导入", "打包",
    "how to", "how do", "how can", "steps",
];

pub(crate) const QUESTION_TERMS: [&str; 18] = [
    "怎么", "如何", "为什么", "为何", "是否", "能不能", "可否", "什么",
    "哪些", "哪个", "多少", "多久", "何时", "何地", "谁", "怎样", "请问", "吗",
];

pub(crate) const TEMPORAL_TERMS: [&str; 24] = [
    "什么时候", "几时", "何日", "几号", "哪天", "日期", "上次", "上回", "最近", "前几天",
    "昨天", "今天", "明天", "上周", "上个月", "几点", "何时", "当时", "之前", "最新",
    "第一次", "首次", "当天", "当日",
];

pub(crate) const FULLWIDTH_QUESTION_MARK: char = '？';

pub(crate) const SENTENCE_PUNCTUATION: [char; 4] = ['。', '！', '？', '；'];

pub(crate) const PHRASE_PUNCTUATION: [char; 3] = ['，', '、', '：'];

pub(crate) const OUTLINE_MARKER: &str = "纲目";
