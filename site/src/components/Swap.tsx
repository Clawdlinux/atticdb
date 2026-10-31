"use client";

import { AnimatePresence, motion, useIsPresent, type Transition } from "motion/react";
import { SWAP, SWAP_RISE } from "@/lib/motion";

type SwapProps = {
  value: string;
  className?: string;
  // popLayout lets the new value take its own width at once; sync stacks both in one grid cell.
  mode?: "popLayout" | "sync";
  // Defaults to the site's SWAP and SWAP_RISE. The installer passes its own quicker pair.
  transition?: Transition;
  rise?: number;
};

type ItemProps = { value: string; className?: string; transition: Transition; rise: number };

function Item({ value, className, transition, rise }: ItemProps) {
  const present = useIsPresent();
  return (
    <motion.span
      className={className}
      data-present={present}
      // The leaving value is decoration: out of the accessible name at once.
      aria-hidden={present ? undefined : true}
      initial={{ opacity: 0, y: rise }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -rise }}
      transition={transition}
    >
      {value}
    </motion.span>
  );
}

/*
 * A short text that rises in when it changes and rises out the old one.
 * The server renders the value at rest: initial={false} skips the entrance on first paint.
 */
export default function Swap({
  value,
  className,
  mode = "popLayout",
  transition = SWAP,
  rise = SWAP_RISE,
}: SwapProps) {
  return (
    <AnimatePresence initial={false} mode={mode}>
      <Item key={value} value={value} className={className} transition={transition} rise={rise} />
    </AnimatePresence>
  );
}
