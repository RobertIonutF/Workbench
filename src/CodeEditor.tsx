import { useEffect, useRef } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { javascript } from '@codemirror/lang-javascript';
import { json } from '@codemirror/lang-json';
import { python } from '@codemirror/lang-python';
import { rust } from '@codemirror/lang-rust';
import { markdown } from '@codemirror/lang-markdown';
import { oneDark } from '@codemirror/theme-one-dark';
import { MergeView } from '@codemirror/merge';
import { EditorState } from '@codemirror/state';
import { EditorView } from '@codemirror/view';

function language(path: string) {
  if (/\.(tsx?|jsx?|mjs|cjs)$/.test(path)) return javascript({ typescript: /tsx?$/.test(path), jsx: /[jt]sx$/.test(path) });
  if (/\.json$/.test(path)) return json();
  if (/\.py$/.test(path)) return python();
  if (/\.rs$/.test(path)) return rust();
  if (/\.(md|markdown)$/.test(path)) return markdown();
  return [];
}
export function CodeEditor({ path, value, onChange, onSelection, readOnly = false }: { path: string; value: string; onChange?: (value: string) => void; onSelection?: (value:string)=>void; readOnly?: boolean }) {
  return <CodeMirror aria-label={`${readOnly ? 'Read' : 'Edit'} ${path}`} value={value} onChange={onChange} onUpdate={update=>{if(update.selectionSet){const range=update.state.selection.main;onSelection?.(update.state.sliceDoc(range.from,range.to));}}} readOnly={readOnly} theme={document.documentElement.dataset.theme === 'light' ? 'light' : oneDark} extensions={[language(path), EditorView.lineWrapping]} height="100%" minHeight="260px" basicSetup={{ lineNumbers: true, foldGutter: true, highlightActiveLine: !readOnly, autocompletion: true, searchKeymap: true }} />;
}
export function SideBySideDiff({ path, before, after }: { path: string; before: string; after: string }) {
  const parent = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!parent.current) return;
    const extensions = [language(path), EditorState.readOnly.of(true), EditorView.editable.of(false), ...(document.documentElement.dataset.theme === 'light' ? [] : [oneDark])];
    const merge = new MergeView({ parent: parent.current, a: { doc: before, extensions }, b: { doc: after, extensions }, highlightChanges: true, gutter: true, collapseUnchanged: { margin: 3, minSize: 8 } });
    return () => merge.destroy();
  }, [path, before, after]);
  return <div className="side-by-side-diff" ref={parent} aria-label={`Before and after ${path}`} />;
}
