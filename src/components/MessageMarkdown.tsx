import { Component, lazy, memo, Suspense, useMemo, type ReactNode } from "react";
import { hasMathMarkdown } from "../markdown-math";
import { MarkdownRenderer, type MessageMarkdownProps } from "./MarkdownRenderer";

// Neither the math plugins nor KaTeX's CSS/fonts are part of the eager graph.
const MathMarkdown = lazy(() => import("./MathMarkdown"));

class MathLoadBoundary extends Component<
  { children: ReactNode; fallback: ReactNode },
  { failed: boolean }
> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    // A failed chunk request must not take the conversation down. Preserve the
    // message as ordinary Markdown until the app is reloaded.
    return this.state.failed ? this.props.fallback : this.props.children;
  }
}

function MessageMarkdownContent(props: MessageMarkdownProps) {
  const hasMath = useMemo(() => hasMathMarkdown(props.body), [props.body]);
  const plain = <MarkdownRenderer {...props} />;
  if (!hasMath) return plain;

  return (
    <MathLoadBoundary fallback={plain}>
      <Suspense fallback={plain}>
        <MathMarkdown {...props} />
      </Suspense>
    </MathLoadBoundary>
  );
}

export const MessageMarkdown = memo(MessageMarkdownContent);
