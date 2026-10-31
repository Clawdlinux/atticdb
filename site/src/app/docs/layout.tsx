import styles from "./docs.module.css";

// Renders inside the root layout's <main>, so the site nav and footer stay.
export default function DocsLayout({ children }: { children: React.ReactNode }) {
  return <div className={styles.docs}>{children}</div>;
}
