"use client";

import type { RevealWindow } from "@/lib/motion";
import { useEffect, useLayoutEffect, useRef, type ElementType, type JSX, type ReactNode } from "react";

type RevealProps = {
  children: ReactNode;
  // Seconds after the element enters the screen.
  delay?: number;
  as?: keyof JSX.IntrinsicElements;
  className?: string;
  id?: string;
};

const useIsoLayoutEffect = typeof window === "undefined" ? useEffect : useLayoutEffect;

/*
 * A section part that rises in once as it enters. The work is done by the inline
 * script from lib/motion.ts, which scans the page before React loads. This hook only
 * hands it elements that mount later, after a client-side navigation.
 */
export default function Reveal({ children, delay = 0, as = "div", className, id }: RevealProps) {
  const ref = useRef<HTMLElement>(null);

  useIsoLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    return (window as RevealWindow).__reveal?.(el);
  }, []);

  const Tag = as as ElementType;
  return (
    <Tag
      ref={ref}
      id={id}
      className={className}
      data-reveal="static"
      data-reveal-delay={delay ? Math.round(delay * 1000) : undefined}
      // The inline script may mark it waiting before React hydrates.
      suppressHydrationWarning
    >
      {children}
    </Tag>
  );
}
