import { lazy, Suspense } from 'react';
const Editor = lazy(() => import('./CodeEditor').then(module => ({ default: module.CodeEditor })));
const Diff = lazy(() => import('./CodeEditor').then(module => ({ default: module.SideBySideDiff })));
export function CodeEditor(props: {path:string;value:string;onChange?:(value:string)=>void;onSelection?:(value:string)=>void;readOnly?:boolean}) {return <Suspense fallback={<p className="field-hint">Loading editor…</p>}><Editor {...props}/></Suspense>;}
export function SideBySideDiff(props: {path:string;before:string;after:string}) {return <Suspense fallback={<p className="field-hint">Loading comparison…</p>}><Diff {...props}/></Suspense>;}
