"use client";

import { useEffect, useRef, type ReactNode } from "react";

type CommandsProps = {
  children: ReactNode;
  className?: string;
  summaryClassName?: string;
};

// A native details: it opens and closes without JS. The script only opens it for a #commands link.
export default function Commands({ children, className, summaryClassName }: CommandsProps) {
  const ref = useRef<HTMLDetailsElement>(null);

  useEffect(() => {
    const openOnHash = () => {
      if (window.location.hash === "#commands" && ref.current) ref.current.open = true;
    };
    openOnHash();
    window.addEventListener("hashchange", openOnHash);
    return () => window.removeEventListener("hashchange", openOnHash);
  }, []);

  return (
    <details ref={ref} id="commands" className={className}>
      <summary className={summaryClassName}>Or pick the commands yourself</summary>
      {children}
    </details>
  );
}
