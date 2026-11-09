export default {
  name: "qoop",
  description: "a portable runtime for human and coding-agent workspaces",
  author: {
    name: "qoop",
    url: "https://github.com/qoop-si/qoop",
  },
  navLinks: [
    { label: "Docs", href: "#introduction" },
    { label: "Keys", href: "#keys" },
    { label: "Automation", href: "#automation" },
    { label: "qoop.si", href: "/" },
    { label: "GitHub", href: "https://github.com/qoop-si/qoop", external: true },
  ],
  sidebar: [
    {
      title: "Getting started",
      children: [
        { title: "Introduction", slug: "introduction" },
        { title: "Install", slug: "install" },
        { title: "Quick start", slug: "quick-start" },
        { title: "Coming from tmux", slug: "coming-from" },
      ],
    },
    {
      title: "Using it",
      children: [
        { title: "Keys", slug: "keys" },
        { title: "Panes and splits", slug: "panes" },
        { title: "Copy mode and search", slug: "copy-mode" },
        { title: "Detach, stop and restore", slug: "sessions" },
        { title: "Sleep", slug: "sleep" },
        { title: "Other machines", slug: "remote" },
        { title: "Your phone", slug: "phone" },
      ],
    },
    {
      title: "Agents",
      children: [
        { title: "Supported agents", slug: "agents" },
        { title: "qass", slug: "qass" },
        { title: "Hooks and skills", slug: "hooks" },
        { title: "Notifications", slug: "notifications" },
        { title: "Automation", slug: "automation" },
      ],
    },
    {
      title: "Reference",
      children: [
        { title: "Configuration", slug: "configuration" },
        { title: "Update and uninstall", slug: "update" },
        { title: "Usage counts", slug: "telemetry" },
        { title: "FAQ and troubleshooting", slug: "faq" },
        { title: "How it is built", slug: "built" },
      ],
    },
  ],
  defaultPage: "introduction",
  footer: {
    left: "AGPL-3.0-or-later",
    right: "qoop",
  },
  // The top of the introduction page. A phone link with an empty url shows as "soon" and
  // links nowhere; give it a url and it turns into a live button. `download` opens it in place.
  home: {
    tagline: "Your coding agents in one terminal, and in your pocket.",
    lede: "It tells you when one needs you. Close the lid; they keep working.",
    install: "curl -fsSL https://qoop.si/install.sh | sh",
    hero: {
      dark: "img/hero-dark.png",
      light: "img/hero-light.png",
      alt: "qoop in a terminal, on an iPhone and on an Android phone",
    },
    phone: [
      {
        label: "Android",
        detail: "download the app (APK, about 56 MB); Google Play soon",
        url: "/releases/latest/qoop-android-arm64.apk",
        download: true,
        icon: "download",
      },
      {
        label: "iPhone",
        detail: "qoop.si/app in the browser, no push notifications yet; App Store soon",
        url: "https://qoop.si/app",
        icon: "apple",
      },
    ],
  },
};
