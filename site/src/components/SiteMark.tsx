import styles from "./SiteMark.module.css";

// The AtticDB mark: a roof with a box kept under it. On load the box rises into place
// and the roof settles over it. Hovering the link lifts the roof like a lid.
export default function SiteMark() {
  return (
    <svg className={styles.mark} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
      <path className={styles.roof} d="M32 3 63 30 55 38.5 32 19 9 38.5 1 30z" />
      <rect className={styles.box} x="19" y="39" width="26" height="22" rx="3" />
    </svg>
  );
}
