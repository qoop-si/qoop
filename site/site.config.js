export default {
  name: "qoop",
  description: "a portable runtime for human and coding-agent workspaces",
  author: {
    name: "us",
    url: "https://github.com/us",
  },
  navLinks: [
    { label: "Docs", href: "#introduction" },
    { label: "Keys", href: "#keys" },
    { label: "Automation", href: "#automation" },
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
        { title: "Sessions and restore", slug: "sessions" },
        { title: "Other machines", slug: "remote" },
      ],
    },
    {
      title: "Agents",
      children: [
        { title: "Supported agents", slug: "agents" },
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
        { title: "FAQ and troubleshooting", slug: "faq" },
      ],
    },
  ],
  defaultPage: "introduction",
  footer: {
    left: "AGPL-3.0-or-later",
    right: "qoop",
  },
  theme: {
    light: { "--accent-gold": "#FF6600", "--accent-gold-dim": "rgba(255, 102, 0, .15)", "--accent-blue": "#FD4B03" },
    dark: { "--accent-gold": "#FF6600", "--accent-gold-dim": "rgba(255, 102, 0, .15)", "--accent-blue": "#FD4B03" },
  },
};
