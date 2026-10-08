import { memo, useRef, useState, type ComponentPropsWithoutRef, type ReactNode } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { IconCheck, IconCopy, IconFile, IconTerminal2 } from "@tabler/icons-react";
import { imageUrl, messageUrl, type Message, type Turn } from "./workspace-data";

function CodeBlock({ children }: { children?: ReactNode }) {
  const ref = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  async function copy() {
    const text = ref.current?.textContent || "";
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true); setFailed(false);
    } catch {
      // Plain HTTP LAN origins do not expose the Clipboard API.
      const selection = window.getSelection();
      if (selection && ref.current) {
        const range = document.createRange(); range.selectNodeContents(ref.current);
        selection.removeAllRanges(); selection.addRange(range);
      }
      setFailed(true);
    }
  }
  return <div className="remote-code"><div className="remote-code-toolbar"><span><IconTerminal2 size={14} />代码</span><button type="button" onClick={() => void copy()} aria-label="复制代码">{copied ? <IconCheck size={14} /> : <IconCopy size={14} />}{failed ? "已选中，请复制" : copied ? "已复制" : "复制"}</button></div><pre ref={ref}>{children}</pre></div>;
}

function MarkdownImage({ src, alt }: ComponentPropsWithoutRef<"img">) {
  const [failed, setFailed] = useState(false);
  const safe = typeof src === "string" ? imageUrl(src) : "";
  return safe && !failed ? <img src={safe} alt={alt || "对话图片"} loading="lazy" referrerPolicy="no-referrer" onError={() => setFailed(true)} /> : <span className="remote-attachment"><IconFile size={16} />{alt || "电脑上的附件"}</span>;
}

export const RichText = memo(function RichText({ text }: { text: string }) {
  return <div className="remote-markdown"><Markdown remarkPlugins={[remarkGfm]} rehypePlugins={[[rehypeHighlight, { detect: false }]]} skipHtml urlTransform={(url, key) => key === "src" ? imageUrl(url) : messageUrl(url)} components={{
    pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
    a: ({ href, children }) => href ? <a href={href} target="_blank" rel="noopener noreferrer">{children}</a> : <span className="remote-file-link">{children}</span>,
    img: ({ src, alt }) => <MarkdownImage src={src} alt={alt} />,
    table: ({ children }) => <div className="remote-table"><table>{children}</table></div>,
  }}>{text}</Markdown></div>;
});

const activityLabels: Record<string, string> = { commandExecution: "执行命令", fileChange: "文件变更", reasoning: "思考摘要", mcpToolCall: "调用工具", dynamicToolCall: "调用工具", webSearch: "搜索网页", plan: "工作计划", collabAgentToolCall: "协作任务" };

function Activity({ message }: { message: Message }) {
  return <details className="remote-activity"><summary>{activityLabels[message.kind] || "工具活动"}{message.status && <span>{message.status === "inProgress" ? "进行中" : message.status === "failed" ? "失败" : ""}</span>}</summary>{message.kind === "reasoning" || message.kind === "plan" ? <RichText text={message.text} /> : <pre>{message.text}</pre>}</details>;
}

export function ConversationTurn({ turn }: { turn: Turn }) {
  const activities = turn.messages.filter(message => message.role === "activity");
  const elapsed = turn.startedAt && turn.completedAt ? Math.max(0, Math.round((turn.completedAt - turn.startedAt) / 1000)) : 0;
  let shownActivities = false;
  return <section className="remote-turn">{turn.messages.map((message, index) => {
    if (message.role === "activity") {
      if (shownActivities) return null;
      shownActivities = true;
      return <details className="remote-work-log" key="work-log"><summary>{turn.status === "inProgress" ? "正在工作" : elapsed ? `用时 ${elapsed >= 60 ? `${Math.floor(elapsed / 60)} 分 ` : ""}${elapsed % 60} 秒` : "工作过程"}<span>{activities.length} 项活动</span></summary>{activities.map((activity, i) => <Activity key={activity.id || i} message={activity} />)}</details>;
    }
    return <article className={`remote-message ${message.role}`} key={message.id || index} aria-label={message.role === "user" ? "你" : "Codex"}>
      {message.role === "user" ? <div className="remote-user-text">{message.text}</div> : <RichText text={message.text} />}
      {message.attachments?.map((attachment, i) => <div className="remote-message-attachment" key={i}><MarkdownImage src={attachment.url} alt={attachment.name} /></div>)}
    </article>;
  })}{turn.status === "failed" && <p className="remote-client-error" role="alert">任务失败：{typeof turn.error === "string" ? turn.error : "请查看电脑端详情"}</p>}{turn.status === "interrupted" && <p className="remote-turn-status">已停止</p>}</section>;
}
