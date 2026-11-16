// What a running app shows to assistive technology, read through the macOS AX API.
//   ax trusted        whether this process may read other apps' AX trees
//   ax dump <pid>     the app's AX tree, one element per line, indented by depth
import ApplicationServices
import Foundation

func value(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var out: CFTypeRef?
    return AXUIElementCopyAttributeValue(element, name as CFString, &out) == .success ? out : nil
}

func text(_ element: AXUIElement, _ name: String) -> String {
    switch value(element, name) {
    case let s as String: return s
    case let n as NSNumber: return n.stringValue
    default: return ""
    }
}

func children(_ element: AXUIElement, _ name: String) -> [AXUIElement] {
    (value(element, name) as? [AXUIElement]) ?? []
}

func dump(_ element: AXUIElement, _ depth: Int) {
    let words = [kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute]
        .map { text(element, $0) }
        .filter { !$0.isEmpty }
        .joined(separator: " | ")
    let role = text(element, kAXRoleAttribute)
    print(String(repeating: "  ", count: depth) + role + (words.isEmpty ? "" : "  " + words))
    guard depth < 40 else { return }
    // The app lists itself among its children: walk its windows instead.
    let next = role == "AXApplication"
        ? children(element, kAXWindowsAttribute) : children(element, kAXChildrenAttribute)
    for child in next where !CFEqual(child, element) { dump(child, depth + 1) }
}

let args = CommandLine.arguments
switch (args.count, args.count > 1 ? args[1] : "") {
case (2, "trusted"):
    print(AXIsProcessTrusted())
case (3, "dump"):
    guard let pid = Int32(args[2]) else { print("not a pid"); exit(2) }
    dump(AXUIElementCreateApplication(pid), 0)
default:
    print("usage: ax trusted | ax dump <pid>")
    exit(2)
}
