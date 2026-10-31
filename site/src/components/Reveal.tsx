"use client";

import { animate } from "motion/react";
import { useEffect, useLayoutEffect, useRef, type ElementType, type JSX, type ReactNode } from "react";

type RevealProps = {
  children: ReactNode;
  delay?: number;
  as?: keyof JSX.IntrinsicElements;
  className?: string;
  id?: string;
};

const RISE_PX = 24;
const DURATION = 0.7;
const EASE = [0.16, 1, 0.3, 1] as const;

const useIsoLayoutEffect = typeof window === "undefined" ? useEffect : useLayoutEffect;

function onScreen(el: HTMLElement) {
  const r = el.getBoundingClientRect();
  return r.top < window.innerHeight && r.bottom > 0;
}

/*
 * Nothing is hidden before JS runs, and nothing on screen at mount is ever hidden.
 * Only an element whose top is below the fold at mount waits, at opacity 0, until
 * it scrolls into view. The observer reveals it; any scroll or resize that finds it
 * on screen reveals it too, so a missed observer callback can't leave it hidden.
 */
export default function Reveal({ children, delay = 0, as = "div", className, id }: RevealProps) {
  const ref = useRef<HTMLElement>(null);

  useIsoLayoutEffect(() => {
    const el = ref.current;
    if (!el || el.getBoundingClientRect().top < window.innerHeight) return;

    const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    el.dataset.reveal = "waiting";
    el.style.opacity = "0";
    if (!still) el.style.transform = `translateY(${RISE_PX}px)`;

    let done = false;
    let failsafe: number | undefined;
    const show = () => {
      if (done) return;
      done = true;
      cleanup();
      el.dataset.reveal = "shown";
      animate(
        el,
        still ? { opacity: [0, 1] } : { opacity: [0, 1], y: [RISE_PX, 0] },
        { duration: still ? 0.3 : DURATION, delay, ease: EASE },
      );
      // If the animation is throttled or dropped, the end state still lands.
      failsafe = window.setTimeout(() => {
        el.style.opacity = "1";
        el.style.transform = "none";
      }, (delay + DURATION) * 1000 + 400);
    };
    const check = () => {
      if (onScreen(el)) show();
    };

    const io =
      typeof IntersectionObserver === "undefined"
        ? undefined
        : new IntersectionObserver(
            (entries) => {
              if (entries.some((e) => e.isIntersecting)) show();
            },
            { threshold: 0.1 },
          );
    io?.observe(el);
    window.addEventListener("scroll", check, { passive: true });
    window.addEventListener("resize", check);

    function cleanup() {
      io?.disconnect();
      window.removeEventListener("scroll", check);
      window.removeEventListener("resize", check);
    }

    return () => {
      cleanup();
      window.clearTimeout(failsafe);
    };
  }, [delay]);

  const Tag = as as ElementType;
  return (
    <Tag ref={ref} id={id} className={className} data-reveal="static">
      {children}
    </Tag>
  );
}
