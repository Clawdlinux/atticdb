"use client";

import { motion } from "motion/react";
import type { JSX, ReactNode } from "react";

type RevealProps = {
  children: ReactNode;
  delay?: number;
  as?: keyof JSX.IntrinsicElements;
  className?: string;
  id?: string;
};

// No `initial` style: server HTML stays visible. `.js [data-reveal]` hides it
// in CSS until it enters view, and the keyframes below set the start values.
export default function Reveal({ children, delay = 0, as = "div", className, id }: RevealProps) {
  const Tag = motion[as as "div"];
  return (
    <Tag
      data-reveal=""
      id={id}
      className={className}
      initial={false}
      whileInView={{ opacity: [0, 1], y: [24, 0] }}
      viewport={{ once: true, amount: 0.3 }}
      transition={{ duration: 0.7, delay, ease: [0.16, 1, 0.3, 1] }}
    >
      {children}
    </Tag>
  );
}
