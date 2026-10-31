// The motion tokens for script. The CSS twins live in globals.css (--ease-out, --t-reveal, --rise).
export const MOTION = {
  // Same curve as --ease-out: a strong ease-out for every entrance and every press.
  ease: "cubic-bezier(0.16, 1, 0.3, 1)",
  revealMs: 800,
  stillMs: 300,
  risePx: 24,
} as const;

// Seconds between the parts of a section: heading, then text, then picture.
export const STAGGER = 0.075;

/*
 * The Framer Motion set for every interactive transition after hydration.
 * One curve (MOTION.ease), one spring with no overshoot, short times for feedback.
 * MotionConfig reducedMotion="user" (Providers) drops the travel and keeps the fades.
 */
export const EASE_OUT = [0.16, 1, 0.3, 1] as const;
export const SPRING = { type: "spring", stiffness: 520, damping: 48, mass: 1 } as const;
export const PRESS = { duration: 0.14, ease: EASE_OUT } as const;
export const SWAP = { duration: 0.24, ease: EASE_OUT } as const;
export const SETTLE = { duration: 0.42, ease: EASE_OUT } as const;
export const SWAP_RISE = 8;
export const TAP = { scale: 0.96 } as const;
export const HOVER = { scale: 1.03 } as const;

/*
 * The installer's set: rigid, corporate, no overshoot. Its own curve, cubic-bezier(0.2, 0, 0, 1).
 * Durations: quick 120 ms, standard 240 ms, slow 480 ms. Exits run 160 ms.
 * The step cascade waits 40 ms a step and stops at 160 ms, so it stays under 200 ms.
 */
export const INSTALL_EASE = [0.2, 0, 0, 1] as const;
export const INSTALL = {
  quick: { duration: 0.12, ease: INSTALL_EASE },
  standard: { duration: 0.24, ease: INSTALL_EASE },
  slow: { duration: 0.48, ease: INSTALL_EASE },
  exit: { duration: 0.16, ease: INSTALL_EASE },
  instant: { duration: 0 },
  risePx: 6,
  stagger: 0.04,
  staggerMax: 0.16,
} as const;

export type RevealWindow = Window & { __reveal?: (el: Element) => () => void };

/*
 * Runs as an inline <head> script, so section entrances never wait for a JS chunk.
 * Nothing is hidden before it runs, and nothing on screen when it runs is ever hidden.
 * Only an element whose top is below the fold waits at opacity 0. The observer shows it,
 * and so does any scroll, resize or print that finds it on screen.
 * Its end state is committed before the entrance plays, so a dropped animation still shows it.
 * Self-contained on purpose: it is serialised with toString().
 */
export function revealBoot(cfg: typeof MOTION) {
  const w = window as RevealWindow;
  if (w.__reveal) return;
  const still = !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
  const seen = new WeakSet<Element>();
  const waiting = new Set<HTMLElement>();

  const onScreen = (el: Element) => {
    const r = el.getBoundingClientRect();
    return r.top < window.innerHeight && r.bottom > 0;
  };

  const forget = (el: Element) => {
    waiting.delete(el as HTMLElement);
    io?.unobserve(el);
  };

  const show = (el: HTMLElement) => {
    if (!waiting.delete(el)) return;
    io?.unobserve(el);
    el.dataset.reveal = "shown";
    el.style.opacity = "";
    el.style.transform = "";
    if (typeof el.animate !== "function") return;
    const end = getComputedStyle(el);
    const from: Keyframe = still
      ? { opacity: 0 }
      : { opacity: 0, transform: `translateY(${cfg.risePx}px)` };
    const to: Keyframe = still ? { opacity: end.opacity } : { opacity: end.opacity, transform: end.transform };
    el.animate([from, to], {
      duration: still ? cfg.stillMs : cfg.revealMs,
      delay: Number(el.dataset.revealDelay) || 0,
      easing: cfg.ease,
      fill: "backwards",
    });
  };

  const check = () => {
    waiting.forEach((el) => {
      if (!el.isConnected) forget(el);
      else if (onScreen(el)) show(el);
    });
  };

  const io =
    typeof IntersectionObserver === "function"
      ? new IntersectionObserver(
          (entries) => {
            for (const e of entries) if (e.isIntersecting) show(e.target as HTMLElement);
          },
          { rootMargin: "0px 0px -8% 0px" },
        )
      : null;

  const add = (el: Element) => {
    if (seen.has(el) || !(el instanceof HTMLElement)) return;
    seen.add(el);
    if (!io || el.getBoundingClientRect().top < window.innerHeight) return;
    el.dataset.reveal = "waiting";
    el.style.opacity = "0";
    if (!still) el.style.transform = `translateY(${cfg.risePx}px)`;
    waiting.add(el);
    io.observe(el);
  };

  // Positions only mean something once the stylesheets apply. The first frame after
  // DOMContentLoaded waits for them, so the scan runs there. Until then nothing is touched.
  let ready = false;
  const scan = () => {
    ready = true;
    document.querySelectorAll("[data-reveal]").forEach(add);
  };
  const start = () => requestAnimationFrame(scan);

  // Returns the unregister for the element, so an unmount drops it from the observer and the checks.
  w.__reveal = (el) => {
    if (ready) add(el);
    return () => forget(el);
  };
  window.addEventListener("scroll", check, { passive: true });
  window.addEventListener("resize", check);
  window.addEventListener("beforeprint", () => waiting.forEach(show));
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
}

export function revealBootScript(): string {
  return `document.documentElement.classList.add('js');(${revealBoot.toString()})(${JSON.stringify(MOTION)});`;
}
