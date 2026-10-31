import type { Metadata } from "next";
import { Bebas_Neue, EB_Garamond, JetBrains_Mono } from "next/font/google"; // taste-skill-ignore: mono is set on pre and code only
import Providers from "@/components/Providers";
import SiteFooter from "@/components/SiteFooter";
import SiteNav from "@/components/SiteNav";
import { pageMetadata } from "@/lib/seo";
import { revealBootScript } from "@/lib/motion";
import { SITE_URL } from "@/lib/site";
import "./globals.css";

const display = Bebas_Neue({
  weight: "400",
  subsets: ["latin"],
  variable: "--font-display",
  display: "swap",
});

const text = EB_Garamond({
  weight: ["400", "500"],
  style: ["normal", "italic"],
  subsets: ["latin"],
  variable: "--font-text",
  display: "swap",
});

const code = JetBrains_Mono({ // taste-skill-ignore: code blocks only
  subsets: ["latin"],
  variable: "--font-code",
  display: "swap",
});

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: "AtticDB",
  openGraph: pageMetadata("/").openGraph,
  twitter: pageMetadata("/").twitter,
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html
      lang="en"
      className={`${display.variable} ${text.variable} ${code.variable}`}
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
        <Providers>
          <a className="button button--accent skip-link" href="#main">
            Skip to content
          </a>
          <SiteNav />
          <main id="main" className="container" tabIndex={-1}>
            {children}
          </main>
          <SiteFooter />
        </Providers>
      </body>
    </html>
  );
}
