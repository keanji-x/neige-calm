/*
 * Drop root-level HTML comment blocks from the markdown AST (#1635 S2b).
 *
 * Why `skipHtml` alone is not enough: react-markdown applies `skipHtml` at
 * the hast level, after mdast-util-to-hast has already put a "\n" text node
 * between every pair of root children. With two consecutive HTML comment
 * blocks — the `<!-- neige:contract … -->` header line, then the prose
 * maintenance contract — both `raw` nodes are removed but the separator
 * survives, so a comment-only prose block renders as a div holding one "\n"
 * text node. CSS `:empty` does not match an element with a whitespace text
 * child, and `.report-block:empty { display: none }` (calm.css) is what keeps
 * the contract block from taking room at the top of every report.
 *
 * Removing the comment nodes at the mdast level, before the separators are
 * inserted, leaves nothing behind. A root-level `html` node whose value
 * starts with `<!--` is exactly one CommonMark HTML block of type 2 (the
 * block ends on the line containing `-->`), so this never drops anything but
 * the comment itself.
 *
 * Deliberately narrow: only root children, only comments. Inline `html` nodes
 * (a comment inside a paragraph) and non-comment raw HTML blocks are left to
 * `skipHtml`, which still drops them.
 */
import type { Root } from 'mdast';

export function remarkDropHtmlComments() {
  return (tree: Root) => {
    tree.children = tree.children.filter(
      (node) => !(node.type === 'html' && node.value.startsWith('<!--')),
    );
  };
}
