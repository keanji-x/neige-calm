// TS JSX shim for `<selectedcontent>` — the customizable-select element
// (Chromium base-select, #891 Workflow drawer). React 19 renders unknown
// lowercase tags as custom elements at runtime; this augmentation only
// teaches the JSX type-checker about it. Remove once @types/react ships
// the element natively.
import 'react';

declare module 'react' {
  namespace JSX {
    interface IntrinsicElements {
      selectedcontent: React.DetailedHTMLProps<
        React.HTMLAttributes<HTMLElement>,
        HTMLElement
      >;
    }
  }
}
