import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

export interface MarkdownPaneProps {
  path: string;
  text: string;
}

export function MarkdownPane({ path, text }: MarkdownPaneProps) {
  return (
    <div
      className="file-viewer-markdown"
      data-wheel-pane="markdown"
      data-path={path}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
    </div>
  );
}
