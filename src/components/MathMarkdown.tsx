import rehypeKatex from "rehype-katex";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import type { PluggableList } from "unified";
import "katex/dist/katex.min.css";
import { MarkdownRenderer, type MessageMarkdownProps } from "./MarkdownRenderer";

const remarkPlugins: PluggableList = [remarkGfm, [remarkMath, { singleDollarTextMath: false }]];
const rehypePlugins: PluggableList = [[rehypeKatex, { strict: false, trust: false }]];

export default function MathMarkdown(props: MessageMarkdownProps) {
  return <MarkdownRenderer {...props} remarkPlugins={remarkPlugins} rehypePlugins={rehypePlugins} />;
}
