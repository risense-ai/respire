import { translations1 } from "./i18n/translations1.js";
import { translations2 } from "./i18n/translations2.js";
import { translations3 } from "./i18n/translations3.js";
export const ZH_TO_EN = [...translations1, ...translations2, ...translations3];

export const EN_TO_ZH = new Map([
  ["Skip to content", "跳到内容"],
  ["Workspace", "工作区"],
  ["Main navigation", "主导航"],
  ["Management", "管理"],
  ["Sidebar layout", "侧栏布局"],
  ["Software UI language; you can also switch it from the top-right corner.", "软件界面语言；右上角也可以快速切换。"],
  ["Overview", "概览"],
  ["Memories", "记忆"],
  ["Diary", "日记"],
  ["Organize", "整理"],
  ["Models & Index", "模型与索引"],
  ["Accounts & Spaces", "账户与空间"],
  ["Install", "安装"],
  ["Access Grants", "访问授权"],
  ["Settings", "设置"],
  ["Updates", "更新"],
  ["Operations", "操作记录"],
  ["Diagnostics", "诊断"],
  ["Local workspace", "本机工作区"],
  ["Language", "语言"],
  ["English", "English"],
  ["Chinese", "中文"],
  ["Refresh", "刷新"],
  ["Theme", "界面主题"],
  ["System", "跟随系统"],
  ["Light", "亮色"],
  ["Dark", "暗色"],
  ["Compact layout", "紧凑布局"],
  ["Connecting to Runtime", "正在连接 Runtime"],
  ["Reading local status", "正在读取本机状态"],
  ["Sync status pending", "同步状态待确认"],
  ["Cause", "前因"],
  ["Action", "行为"],
  ["Result", "后果"],
  ["Trigger", "触发"],
]);

const PHRASES = [
  [/显示其余 (\d+) 个/g, "Show $1 more", "显示其余 $1 个"],
  [/收起 (\d+) 个/g, "Fold $1", "收起 $1 个"],
  [/(\d[\d,]*) 条记忆/g, "$1 memories", "$1 条记忆"],
  [/总(计)? (\d[\d,]*) 条，包含已删除记录/g, "Total $2 records, including deleted records", "总$1 $2 条，包含已删除记录"],
  [/(\d[\d,]*) 项冲突/g, "$1 conflicts", "$1 项冲突"],
  [/已载入 (.+?) \/ (.+?) 条 · 已选 (.+?) 条/g, "Loaded $1 / $2 · selected $3", "已载入 $1 / $2 条 · 已选 $3 条"],
  [/检索返回 (.+?) 条；搜索覆盖整库/g, "Retrieved $1; search covers all memories", "检索返回 $1 条；搜索覆盖整库"],
  [/图谱显示 (\d[\d,]*) 个已载入节点/g, "Graph shows $1 loaded nodes", "图谱显示 $1 个已载入节点"],
  [/显示第 (\d+)–(\d+) 条，共 ([\d,]+) 条记忆/g, "Showing $1–$2 of $3 memories", "显示第 $1–$2 条，共 $3 条记忆"],
  [/第 (\d+) \/ (\d+) 页 · 共 ([\d,]+) 条/g, "Page $1 / $2 · $3 total", "第 $1 / $2 页 · 共 $3 条"],
  [/本页之外 (\d+) 条关系：(.+)/g, "$2 · $1 relations outside this page", "本页之外 $1 条关系：$2"],
  [/再显示 (.+?) 条/g, "Show $1 more", "再显示 $1 条"],
  [/耗时 (.+?) ms · 返回 (.+?) 条/g, "Elapsed $1 ms · returned $2", "耗时 $1 ms · 返回 $2 条"],
  [/正在验证第 (.+?) \/ 9 轮，完成 (.+?) 轮。/g, "Verifying round $1 / 9; completed $2 rounds.", "正在验证第 $1 / 9 轮，完成 $2 轮。"],
  [/请求失败 \((\d+)\)/g, "Request failed ($1)", "请求失败 ($1)"],
  [/请求 (.+?) 的结果未确认。请在操作记录中查询，不要重复提交。/g, "The result for request $1 is unconfirmed. Check Operations; do not submit it again.", "请求 $1 的结果未确认。请在操作记录中查询，不要重复提交。"],
  [/请求 (.+?) 的回执读取失败，结果尚未确认。请到操作记录查询原请求，不要重复提交。/g, "Reading the receipt for request $1 failed; the result is unconfirmed. Check the original request in Operations; do not submit it again.", "请求 $1 的回执读取失败，结果尚未确认。请到操作记录查询原请求，不要重复提交。"],
  [/^(\d+) 个来源$/g, "$1 sources", "$1 个来源"],
  [/^已检测到 · (\d+) 个存储位置$/g, "Detected · $1 storage location(s)", "已检测到 · $1 个存储位置"],
  [/按 Harness：(.+)/g, "By harness: $1", "按 Harness：$1"],
  [/^导入 ([\d,]+) 条预览会话$/g, "Import $1 previewed sessions", "导入 $1 条预览会话"],
  [/^每源上限必须是 1–(\d+) 的整数$/g, "Per-harness limit must be an integer from 1 to $1", "每源上限必须是 1–$1 的整数"],
  [/路径含「(.+?)」/g, "path contains \"$1\"", "路径含「$1」"],
  [/模型含「(.+?)」/g, "model contains \"$1\"", "模型含「$1」"],
  [/^每源上限 ([\d,]*)$/g, "limit $1 per harness", "每源上限 $1"],
  [/^条件：(.+)。预览时待导入 ([\d,]+) 条；执行时 CLI 会重新扫描同条件，已导入会话自动跳过，结果以实际回执为准。确认导入？$/g, "Conditions: $1. The preview showed $2 pending; the CLI rescans the same conditions at execution time, already-imported sessions are skipped, and the actual receipt is authoritative. Confirm import?", "条件：$1。预览时待导入 $2 条；执行时 CLI 会重新扫描同条件，已导入会话自动跳过，结果以实际回执为准。确认导入？"],
  [/^已选 (\d+) \/ (\d+) 个$/g, "$1 of $2 selected", "已选 $1 / $2 个"],
  [/^已导入 ([\d,]+) 条$/g, "$1 imported", "已导入 $1 条"],
  [/已启用 (.+?) 模式/g, "Enabled $1 mode", "已启用 $1 模式"],
  [/上次验证 (.+)/g, "Last verified $1", "上次验证 $1"],
  [/已切换使用 (.+?) · (.+?)，真实推理验证通过。/g, "Switched to $1 · $2; the real inference check passed.", "已切换使用 $1 · $2，真实推理验证通过。"],
  [/^已注入且最新/g, "Injected and current", "已注入且最新"],
  [/^已注入但过期/g, "Injected but outdated", "已注入但过期"],
  [/^未注入/g, "Not injected", "未注入"],
  [/^应用未检测到/g, "App not detected", "应用未检测到"],
  [/^插件源目录已就绪/g, "Plugin source directory ready", "插件源目录已就绪"],
  [/^插件未安装/g, "Plugin not installed", "插件未安装"],
  [/^注入 (.+) 提示词$/g, "Inject $1 prompt", "注入 $1 提示词"],
  [/^安装 (.+) 插件$/g, "Install $1 plugin", "安装 $1 插件"],
  [/^将写入 (.+?) 的提示词。$/g, "The prompt will be written to $1.", "将写入 $1 的提示词。"],
  [/^将按当前模式更新 (.+?) 的提示词。$/g, "The prompt for $1 will be updated in the current mode.", "将按当前模式更新 $1 的提示词。"],
  [/^将移除 (.+?) 中的注入内容。$/g, "The injected content in $1 will be removed.", "将移除 $1 中的注入内容。"],
  [/^启用 (.+)$/g, "Enable $1", "启用 $1"],
];

let language = "en";

const lookup = () => (language === "zh" ? new Map(EN_TO_ZH) : new Map(ZH_TO_EN));

function replacePhrases(text) {
  let result = text;
  for (const [pattern, en, zh] of PHRASES) result = result.replace(pattern, language === "zh" ? zh : en);
  return result;
}

function translateText(node, dictionary) {
  if (node.closest?.("pre,textarea,.reading-content,.memory-title")) return;
  const original = node.nodeValue;
  if (!original.trim()) return;
  const trimmed = original.trim();
  const translated = dictionary.get(trimmed);
  if (translated && translated !== trimmed) {
    node.nodeValue = original.replace(trimmed, translated);
    return;
  }
  const replaced = replacePhrases(original);
  if (replaced !== original) node.nodeValue = replaced;
}

function translateAttributes(root, dictionary) {
  for (const element of root.querySelectorAll("[title],[placeholder],[aria-label]")) {
    if (element.matches?.(".memory-title")) continue;
    for (const name of ["title", "placeholder", "aria-label"]) {
      const value = element.getAttribute(name);
      if (!value) continue;
      const translated = dictionary.get(value.trim()) || replacePhrases(value);
      if (translated !== value) element.setAttribute(name, translated);
    }
  }
}

function walkText(root, callback) {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) callback(walker.currentNode);
}

export function applyTranslations(root = document) {
  if (root === document) document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
  const dictionary = lookup();
  walkText(root, (node) => translateText(node, dictionary));
  if (root !== document && root.nodeType === Node.ELEMENT_NODE) translateAttributes(root, dictionary);
  else translateAttributes(document.body, dictionary);
}

export function setLanguage(value) {
  language = value === "zh" ? "zh" : "en";
  localStorage.setItem("respire-language", language);
  applyTranslations();
}

export const currentLanguage = () => language;
export const t = (text) => language === "zh" ? (EN_TO_ZH.get(text) || text) : (new Map(ZH_TO_EN).get(text) || text);
