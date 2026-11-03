import type { Metadata } from "next";
import { Bebas_Neue, EB_Garamond, JetBrains_Mono } from "next/font/google"; // taste-skill-ignore: mono is set on pre and code only
import SiteFooter from "@/components/SiteFooter";
import SiteNav from "@/components/SiteNav";
import { pageMetadata, RSS_FEED } from "@/lib/seo";
import { revealBootScript } from "@/lib/motion";
import { SITE_URL } from "@/lib/site";
import "./globals.css";

const display = Bebas_Neue({
  weight: "400",
  subsets: ["latin"],
  variable: "--font-display",
  display: "swap",
});

// Only the faces on the first screen preload: Bebas Neue, EB Garamond roman and JetBrains Mono
// (the install command in the hero, code at the top of the docs).
// The italic is its own call with the same family name and variable, so it joins the same
// family but is fetched only when a page sets italic text.
const text = EB_Garamond({
  weight: ["400", "500"],
  style: "normal",
  subsets: ["latin"],
  variable: "--font-text",
  display: "swap",
});

const textItalic = EB_Garamond({
  weight: ["400", "500"],
  style: "italic",
  subsets: ["latin"],
  variable: "--font-text",
  display: "swap",
  preload: false,
});

const code = JetBrains_Mono({ // taste-skill-ignore: code blocks only
  subsets: ["latin"],
  variable: "--font-code",
  display: "swap",
});

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: "AtticDB",
  alternates: { types: RSS_FEED },
  openGraph: pageMetadata("/").openGraph,
  twitter: pageMetadata("/").twitter,
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html
      lang="en"
      className={`${display.variable} ${text.variable} ${textItalic.variable} ${code.variable}`}
      suppressHydrationWarning
    >
      <head>
        <script
          dangerouslySetInnerHTML={{
            __html: revealBootScript(),
          }}
        />
      </head>
      <body>
        <a className="button button--accent skip-link" href="#main">
          Skip to content
        </a>
        <SiteNav />
        <main id="main" className="container" tabIndex={-1}>
          {children}
        </main>
        <SiteFooter />
      </body>
    </html>
  );
}
