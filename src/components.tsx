import { useEffect, useId, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from 'react';
import { post } from './api';
import { AlertCircle, ArrowUpRight, Check, ChevronDown, LoaderCircle, X } from 'lucide-react';

export function Button({ children, className = '', tone = 'default', loading, ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { tone?: 'default' | 'primary' | 'ghost' | 'danger'; loading?: boolean }) {
  return <button className={`button ${tone} ${className}`} {...props} disabled={props.disabled || loading}>{loading && <LoaderCircle size={15} className="spin" />}{children}</button>;
}
export function IconButton({ label, children, className = '', ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return <button className={`icon-button ${className}`} title={label} aria-label={label} {...props}>{children}</button>;
}
export function Field({ label, hint, children, className = '' }: { label: string; hint?: string; children: ReactNode; className?: string }) {
  const id = useId();
  return <div className={`field ${className}`}><label htmlFor={id}>{label}</label><div className="field-control" id={id}>{children}</div>{hint && <p className="field-hint">{hint}</p>}</div>;
}
export function Empty({ icon, title, children, action }: { icon?: ReactNode; title: string; children?: ReactNode; action?: ReactNode }) {
  return <div className="empty-state">{icon && <div className="empty-icon">{icon}</div>}<h3>{title}</h3>{children && <p>{children}</p>}{action && <div className="empty-actions">{action}</div>}</div>;
}
export function ErrorNotice({ children, onDismiss }: { children: ReactNode; onDismiss?: () => void }) {
  return <div className="notice error" role="alert"><AlertCircle size={17} /><span>{children}</span>{onDismiss && <IconButton label="Dismiss error" onClick={onDismiss}><X size={15} /></IconButton>}</div>;
}
export function Badge({ children, kind = '' }: { children: ReactNode; kind?: string }) { return <span className={`badge ${kind}`}>{children}</span>; }
export function Status({ ok, children }: { ok: boolean; children: ReactNode }) { return <span className={`status ${ok ? 'good' : 'muted'}`}>{ok ? <Check size={13} /> : <span className="status-dot" />}{children}</span>; }
export function Diagnostics({ items }: { items: string[] }) { return items.length > 0 ? <ul className="diagnostics">{items.map((item, i) => <li key={i}>{item}</li>)}</ul> : null; }
export function Select({ children, ...props }: React.SelectHTMLAttributes<HTMLSelectElement>) { return <span className="select-wrap"><select {...props}>{children}</select><ChevronDown size={13} /></span>; }
export function ExternalLabel({ children }: { children: ReactNode }) { return <>{children}<ArrowUpRight size={13} /></>; }

export function Modal({ title, subtitle, children, onClose, wide }: { title: string; subtitle?: string; children: ReactNode; onClose: () => void; wide?: boolean }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => { dialog.current?.showModal(); return () => dialog.current?.close(); }, []);
  return <dialog ref={dialog} className={`modal ${wide ? 'wide' : ''}`} aria-labelledby={titleId} onCancel={(event) => { event.preventDefault(); onClose(); }} onClick={(event) => { if (event.target === dialog.current) onClose(); }}><div className="modal-header"><div><h2 id={titleId}>{title}</h2>{subtitle && <p>{subtitle}</p>}</div><IconButton label="Close dialog" onClick={onClose}><X size={18} /></IconButton></div>{children}</dialog>;
}

export function PathInput({ value, onChange, placeholder, directory = true }: { value: string; onChange: (v: string) => void; placeholder?: string; directory?: boolean }) {
  const [picking, setPicking] = useState(false);
  const [error, setError] = useState('');
  async function pick() {
    setPicking(true); setError('');
    try {
      const result = await post<{ path?: string }>('/api/system/pick-directory');
      if (result.path) onChange(result.path);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setPicking(false); }
  }
  return <><div className="input-action"><input aria-label={directory ? 'Folder path' : 'File or folder path'} value={value} onChange={(event) => onChange(event.target.value)} placeholder={placeholder} autoComplete="off" spellCheck={false} />{directory && <Button type="button" onClick={pick} loading={picking}>Browse</Button>}</div>{error && <p className="field-hint error-text">{error} You can enter the path manually.</p>}</>;
}
