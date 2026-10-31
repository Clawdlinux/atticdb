"use client";

import { AnimatePresence, motion, useIsPresent } from "motion/react";
import { SWAP, SWAP_RISE } from "@/lib/motion";

type SwapProps = {
  value: string;
  className?: string;
  // popLayout lets the new value take its own width at once; sync stacks both in one grid cell.
  mode?: "popLayout" | "sync";
};

function Item({ value, className }: { value: string; className?: string }) {
  const present = useIsPresent();
  return (
    <motion.span
      className={className}
      data-present={present}
      // The leaving value is decoration: out of the accessible name at once.
      aria-hidden={present ? undefined : true}
      initial={{ opacity: 0, y: SWAP_RISE }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -SWAP_RISE }}
      transition={SWAP}
    >
      {value}
    </motion.span>
  );
}

/*
 * A short text that rises in when it changes and rises out the old one.
 * The server renders the value at rest: initial={false} skips the entrance on first paint.
 */
export default function Swap({ value, className, mode = "popLayout" }: SwapProps) {
  return (
    <AnimatePresence initial={false} mode={mode}>
      <Item key={value} value={value} className={className} />
    </AnimatePresence>
  );
}
