// The notch: qoro's agents at the top of the screen.
//
// Closed, it is the camera housing grown two ears: the person's pet on the left with
// an amber badge counting the agents that need them, and on the right how many are
// working and idle. Clicked, it drops an inbox of every agent, grouped by what it is
// doing; clicking a row switches the qoro interface to that terminal and brings its
// window forward. With no qoro running, or no agents in it, the notch is left alone.
//
// Everything it knows comes from the `qoro` binary that started it, named as the
// first argument by `qoro notch run`: `agents --watch` for the rows, `avatar show
// --json` for the pet, `focus` for a click. It links no qoro code and reads no qoro
// file, so the command line stays the one contract.

import AppKit
import SwiftUI

let qoro = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "qoro"

// MARK: model

struct Row: Decodable, Equatable, Identifiable {
    let terminal: Int
    let name: String?
    /// Null while a terminal is still starting, which must not drop the whole line.
    let program: String?
    let state: String
    let agent: Bool
    let agent_title: String?
    let cwd: String?

    var id: Int { terminal }

    enum Kind { case waiting, working, idle }

    var kind: Kind {
        switch state {
        case "blocked", "failed": return .waiting
        case "working": return .working
        default: return .idle
        }
    }

    var label: String { name ?? program ?? "terminal" }
    var place: String { cwd.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "" }
    var detail: String {
        if state == "failed" { return "failed" }
        return agent_title ?? (kind == .waiting ? "needs you" : "")
    }
}

struct Pet: Decodable {
    let pixels: [[String?]]
}

@MainActor
final class Store: ObservableObject {
    @Published var rows: [Row] = []
    @Published var pixels: [[Color?]] = []
    @Published var open = false
    @Published var hover = false
    /// Bumped each time an agent finishes a turn, so the pet can cheer once.
    @Published var cheers = 0
    /// Told after every change of rows, so the panel re-reads where its shape now is
    /// without waiting for the mouse to move.
    var changed: (() -> Void)?

    var waiting: Int { rows.filter { $0.kind == .waiting }.count }
    var working: Int { rows.filter { $0.kind == .working }.count }
    var idle: Int { rows.filter { $0.kind == .idle }.count }

    /// The right ear in words, zeros left out: what is drawn, and what its width is measured by.
    var countsText: String {
        [working > 0 ? "\(working) working" : nil, idle > 0 ? "\(idle) idle" : nil]
            .compactMap { $0 }.joined(separator: " \u{00b7} ")
    }

    func take(_ next: [Row]) {
        let before = Dictionary(rows.map { ($0.terminal, $0.kind) }, uniquingKeysWith: { a, _ in a })
        let finished = next.contains { before[$0.terminal] == .working && $0.kind == .idle }
        // A slept terminal is a remembered row with no process behind it: nothing to
        // count, and nothing a click could show.
        let agents = next.filter { $0.agent && $0.state != "sleeping" }
        if agents != rows { rows = agents }
        if finished { cheers += 1 }
        if rows.isEmpty { open = false }
        changed?()
    }
}

// MARK: qoro

/// Run qoro once and hand back what it printed, or nil when it failed.
func runQoro(_ args: [String]) -> Data? {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: qoro)
    p.arguments = args
    let out = Pipe()
    p.standardOutput = out
    p.standardError = FileHandle.nullDevice
    do { try p.run() } catch { return nil }
    let data = out.fileHandleForReading.readDataToEndOfFile()
    p.waitUntilExit()
    return p.terminationStatus == 0 ? data : nil
}

func hex(_ s: String) -> Color? {
    guard s.hasPrefix("#"), let v = UInt32(s.dropFirst(), radix: 16) else { return nil }
    return Color(red: Double(v >> 16 & 0xff) / 255, green: Double(v >> 8 & 0xff) / 255, blue: Double(v & 0xff) / 255)
}

/// The pet, trimmed to the pixels it wears. Read once at start and again whenever the
/// inbox opens, so a pet picked meanwhile shows without a restart.
func loadPet(into store: Store) {
    DispatchQueue.global().async {
        guard let data = runQoro(["avatar", "show", "--json"]),
              let pet = try? JSONDecoder().decode(Pet.self, from: data) else { return }
        var grid = pet.pixels.map { $0.map { $0.flatMap(hex) } }
        grid.removeAll { $0.allSatisfy { $0 == nil } }
        let cols = (grid.first?.indices ?? 0..<0).filter { x in grid.contains { $0[x] != nil } }
        if let lo = cols.first, let hi = cols.last { grid = grid.map { Array($0[lo...hi]) } }
        DispatchQueue.main.async { store.pixels = grid }
    }
}

/// Follow `qoro agents --watch --json`: one JSON array per line, a line each time
/// anything an agent row shows changes. When qoro is not running the command exits;
/// it is started again a few seconds later, which is the only waiting this does.
func watch(into store: Store) {
    Thread.detachNewThread {
        while true {
            // A pool per run: this thread never ends, so without one every Process, Pipe
            // and chunk it ever made would stay allocated. Measured, about 19 KB a round.
            autoreleasepool { watchOnce(into: store) }
            DispatchQueue.main.async { store.take([]) }
            Thread.sleep(forTimeInterval: 3)
        }
    }
}

/// One run of the watch, until qoro closes it.
func watchOnce(into store: Store) {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: qoro)
    p.arguments = ["agents", "--watch", "--json"]
    let out = Pipe()
    p.standardOutput = out
    p.standardError = FileHandle.nullDevice
    guard (try? p.run()) != nil else { return }
    var buffer = Data()
    let handle = out.fileHandleForReading
    while case let chunk = autoreleasepool(invoking: { handle.availableData }), !chunk.isEmpty {
        buffer.append(chunk)
        while let nl = buffer.firstIndex(of: 0x0a) {
            let line = buffer[buffer.startIndex..<nl]
            buffer.removeSubrange(buffer.startIndex...nl)
            if let rows = try? JSONDecoder().decode([Row].self, from: line) {
                DispatchQueue.main.async { store.take(rows) }
            }
        }
    }
    p.waitUntilExit()
}

/// Switch the interface to a terminal, then raise the app that interface runs in: the
/// first process up its parent chain that is an app the window server knows.
func jump(to terminal: Int) {
    DispatchQueue.global().async {
        guard let data = runQoro(["focus", String(terminal), "--json"]),
              let reply = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let pids = reply["clients"] as? [Int] else { return }
        DispatchQueue.main.async {
            // Opened rather than `activate()`d: since macOS 14 an app in the background may
            // not hand activation to another one, and the notch is always in the background.
            guard let url = pids.lazy.compactMap({ hostApp(of: pid_t($0))?.bundleURL }).first else { return }
            let config = NSWorkspace.OpenConfiguration()
            config.activates = true
            NSWorkspace.shared.openApplication(at: url, configuration: config)
        }
    }
}

func parent(of pid: pid_t) -> pid_t? {
    var info = kinfo_proc()
    var size = MemoryLayout<kinfo_proc>.stride
    var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid]
    guard sysctl(&mib, 4, &info, &size, nil, 0) == 0, size > 0 else { return nil }
    let ppid = info.kp_eproc.e_ppid
    return ppid > 1 ? ppid : nil
}

func hostApp(of pid: pid_t) -> NSRunningApplication? {
    var cur: pid_t? = pid
    while let p = cur {
        if let app = NSRunningApplication(processIdentifier: p), app.activationPolicy == .regular {
            return app
        }
        cur = parent(of: p)
    }
    return nil
}

// MARK: views

let amber = Color(red: 0.984, green: 0.749, blue: 0.141)
let spring = Animation.spring(response: 0.42, dampingFraction: 0.74)

/// The concave-shouldered shape of the camera housing, grown to any size.
struct NotchShape: Shape {
    var radius: CGFloat
    var animatableData: CGFloat {
        get { radius }
        set { radius = newValue }
    }

    func path(in r: CGRect) -> Path {
        let s: CGFloat = 6
        var p = Path()
        p.move(to: CGPoint(x: r.minX, y: r.minY))
        p.addQuadCurve(to: CGPoint(x: r.minX + s, y: r.minY + s), control: CGPoint(x: r.minX + s, y: r.minY))
        p.addLine(to: CGPoint(x: r.minX + s, y: r.maxY - radius))
        p.addQuadCurve(to: CGPoint(x: r.minX + s + radius, y: r.maxY), control: CGPoint(x: r.minX + s, y: r.maxY))
        p.addLine(to: CGPoint(x: r.maxX - s - radius, y: r.maxY))
        p.addQuadCurve(to: CGPoint(x: r.maxX - s, y: r.maxY - radius), control: CGPoint(x: r.maxX - s, y: r.maxY))
        p.addLine(to: CGPoint(x: r.maxX - s, y: r.minY + s))
        p.addQuadCurve(to: CGPoint(x: r.maxX, y: r.minY), control: CGPoint(x: r.maxX - s, y: r.minY))
        p.closeSubpath()
        return p
    }
}

struct PetView: View {
    let pixels: [[Color?]]
    let px: CGFloat

    var body: some View {
        Canvas { ctx, _ in
            for (y, row) in pixels.enumerated() {
                for (x, c) in row.enumerated() {
                    if let c { ctx.fill(Path(CGRect(x: CGFloat(x) * px, y: CGFloat(y) * px, width: px, height: px)), with: .color(c)) }
                }
            }
        }
        .frame(width: CGFloat(pixels.first?.count ?? 0) * px, height: CGFloat(pixels.count) * px)
    }
}

/// The pet as it sits in the left ear: hopping while an agent works, jumping while one
/// waits, a single bounce when one finishes, still otherwise.
///
/// The hop is two poses swapped a few times a second, the way the sidebar's pet art
/// moves, rather than an animation interpolated every frame: a repeating SwiftUI
/// animation redraws at the display's rate and cost three percent of a core measured.
struct LivePet: View {
    @ObservedObject var s: Store
    let px: CGFloat
    @State private var cheer = false
    @Environment(\.accessibilityReduceMotion) private var still

    var body: some View {
        let waiting = s.waiting > 0
        Group {
            if !still, waiting || s.working > 0 {
                let beat = waiting ? 0.45 : 0.32
                TimelineView(.periodic(from: .now, by: beat)) { t in
                    let up = Int(t.date.timeIntervalSinceReferenceDate / beat) % 2 == 0
                    PetView(pixels: s.pixels, px: px)
                        .offset(y: up ? (waiting ? -3 : -1.5) : 0)
                        .rotationEffect(.degrees(up && waiting ? -6 : 0))
                }
            } else {
                PetView(pixels: s.pixels, px: px)
            }
        }
        .scaleEffect(cheer ? 1.18 : 1)
        .onChange(of: s.cheers) { _, _ in
            guard !still else { return }
            withAnimation(.spring(response: 0.25, dampingFraction: 0.4)) { cheer = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { withAnimation(spring) { cheer = false } }
        }
    }
}

struct Badge: View {
    let n: Int
    var body: some View {
        Text("\(n)")
            .font(.system(size: 9, weight: .bold))
            .foregroundStyle(.black)
            .contentTransition(.numericText())
            .padding(.horizontal, 3.5)
            .frame(minWidth: 14, minHeight: 14)
            .background(Capsule().fill(amber))
            .overlay(Capsule().stroke(.black, lineWidth: 2))
            .scaleEffect(n > 0 ? 1 : 0.01)
            .opacity(n > 0 ? 1 : 0)
            .accessibilityLabel("\(n) need you")
    }
}

struct Counts: View {
    @ObservedObject var s: Store
    var body: some View {
        HStack(spacing: 4) {
            if s.working > 0 {
                Text("\(s.working) working").foregroundStyle(.white).contentTransition(.numericText())
            }
            if s.working > 0, s.idle > 0 { Text("\u{00b7}").foregroundStyle(.white.opacity(0.35)) }
            if s.idle > 0 {
                Text("\(s.idle) idle").foregroundStyle(.white.opacity(0.5)).contentTransition(.numericText())
            }
        }
        .font(.system(size: 12, weight: .medium))
        .lineLimit(1)
        .fixedSize()
    }
}

struct RowView: View {
    let row: Row
    @State private var over = false

    var body: some View {
        HStack(spacing: 6) {
            Text(row.label)
                .fontWeight(.medium)
                .foregroundStyle(row.kind == .waiting ? amber : row.kind == .idle ? .white.opacity(0.6) : .white)
            Text([row.place, row.detail].filter { !$0.isEmpty }.joined(separator: " \u{00b7} "))
                .foregroundStyle(.white.opacity(0.42))
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 8)
            Text(over ? "open" : "\(row.terminal)")
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.white.opacity(over ? 0.9 : 0.4))
        }
        .font(.system(size: 13))
        .padding(.horizontal, 8)
        .frame(height: 30)
        .background(RoundedRectangle(cornerRadius: 8).fill(
            row.kind == .waiting ? amber.opacity(over ? 0.18 : 0.1) : .white.opacity(over ? 0.08 : 0)))
        .contentShape(Rectangle())
        .onHover { over = $0 }
    }
}

struct Island: View {
    @ObservedObject var s: Store
    let pick: (Int) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                PetView(pixels: s.pixels, px: 2)
                Text(NSFullUserName().split(separator: " ").first.map(String.init)?.lowercased() ?? NSUserName())
                    .font(.system(size: 13, weight: .semibold)).foregroundStyle(.white)
                Spacer()
                HStack(spacing: 12) {
                    Text("\(s.waiting) waiting").foregroundStyle(amber)
                    Text("\(s.working) working").foregroundStyle(.white)
                    Text("\(s.idle) idle").foregroundStyle(.white.opacity(0.45))
                }
                .font(.system(size: 12, weight: .medium, design: .monospaced))
            }
            .padding(.vertical, 8)
            Divider().overlay(.white.opacity(0.08)).padding(.bottom, 2)
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 0) {
                    section("needs you", .waiting)
                    section("working", .working)
                    section("idle", .idle)
                }
                // Room for the rows' highlight, which reaches past the text on both sides.
                .padding(.horizontal, 8)
            }
            .padding(.horizontal, -8)
        }
        .padding(.horizontal, 20)
        .padding(.bottom, 14)
    }

    @ViewBuilder
    private func section(_ title: String, _ kind: Row.Kind) -> some View {
        let rows = s.rows.filter { $0.kind == kind }
        if !rows.isEmpty {
            Text(title.uppercased())
                .font(.system(size: 10.5, design: .monospaced))
                .tracking(0.6)
                .foregroundStyle(.white.opacity(0.38))
                .padding(.top, 8).padding(.bottom, 3)
            ForEach(rows) { row in
                RowView(row: row)
                    .padding(.horizontal, -8)
                    .onTapGesture { pick(row.terminal) }
                    .accessibilityElement(children: .combine)
                    .accessibilityAddTraits(.isButton)
                    .accessibilityHint("Shows terminal \(row.terminal)")
                    .accessibilityAction { pick(row.terminal) }
            }
        }
    }
}

/// Sizes the panel reads to know where clicks belong to the notch.
@MainActor
struct Geometry {
    var notch: CGSize
    static let openWidth: CGFloat = 500
    static let rowHeight: CGFloat = 30
    /// The tallest the inbox grows; past it the list scrolls. Inside the panel's 520.
    static let openMaxHeight: CGFloat = 460
    /// The left ear holds the pet and the badge beside it, whatever the right one says.
    static let petEar: CGFloat = 62

    func closedWidth(_ s: Store) -> CGFloat {
        guard !s.rows.isEmpty else { return notch.width }
        let text = NSAttributedString(
            string: s.countsText,
            attributes: [.font: NSFont.systemFont(ofSize: 12, weight: .medium)])
        let ear = max(Geometry.petEar, ceil(text.size().width) + 26)
        return notch.width + 2 * ear
    }

    func openHeight(_ s: Store) -> CGFloat {
        let sections = [Row.Kind.waiting, .working, .idle].filter { k in s.rows.contains { $0.kind == k } }.count
        let full = notch.height + 40 + CGFloat(sections) * 25 + CGFloat(s.rows.count) * Geometry.rowHeight + 4
        return min(full, Geometry.openMaxHeight)
    }

    func size(_ s: Store) -> CGSize {
        s.open
            ? CGSize(width: Geometry.openWidth, height: openHeight(s))
            : CGSize(width: closedWidth(s) + (s.hover && !s.rows.isEmpty ? 8 : 0), height: notch.height + (s.hover && !s.rows.isEmpty ? 3 : 0))
    }
}

struct NotchView: View {
    @ObservedObject var s: Store
    let g: Geometry
    let pick: (Int) -> Void

    var body: some View {
        let size = g.size(s)
        let shown = !s.rows.isEmpty
        ZStack(alignment: .top) {
            // Nothing to show draws nothing: on a Mac without a notch there is no housing
            // to hide behind, and a black pill with no agents would just be in the way.
            NotchShape(radius: s.open ? 22 : 10).fill(.black).opacity(shown ? 1 : 0)
            VStack(spacing: 0) {
                HStack(spacing: 0) {
                    ZStack(alignment: .topTrailing) {
                        LivePet(s: s, px: 2.5)
                        Badge(n: s.waiting).offset(x: 14, y: -5)
                    }
                    .padding(.leading, 20)
                    Spacer(minLength: g.notch.width)
                    Counts(s: s).padding(.trailing, 20)
                }
                .frame(height: g.notch.height)
                .opacity(shown ? 1 : 0)
                if s.open {
                    Island(s: s, pick: pick)
                        .transition(.opacity.combined(with: .offset(y: -10)))
                }
            }
        }
        .frame(width: size.width, height: size.height)
        .clipped()
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .animation(spring, value: s.open)
        .animation(spring, value: s.hover)
        .animation(spring, value: s.rows)
    }
}

// MARK: panel

final class NotchPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class App: NSObject, NSApplicationDelegate {
    let store = Store()
    var panel: NotchPanel!
    var geometry = Geometry(notch: CGSize(width: 185, height: 32))
    var monitors: [Any] = []
    var closing = false

    func applicationDidFinishLaunching(_ note: Notification) {
        panel = NotchPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.level = .statusBar + 1
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        panel.ignoresMouseEvents = true
        place()
        NotificationCenter.default.addObserver(forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.place() }
        }
        panel.orderFrontRegardless()

        // Clicks outside the notch's current shape go to whatever is under it: the panel
        // takes the mouse only while the pointer is over the black.
        let move: (NSEvent) -> Void = { [weak self] _ in self?.track() }
        monitors.append(NSEvent.addGlobalMonitorForEvents(matching: [.mouseMoved, .leftMouseDragged], handler: move) as Any)
        monitors.append(NSEvent.addLocalMonitorForEvents(matching: [.mouseMoved, .leftMouseDown]) { [weak self] e in
            self?.track()
            if e.type == .leftMouseDown, let self, !self.store.open, self.inside() {
                self.toggle()
            }
            return e
        } as Any)
        monitors.append(NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] _ in
            if let self, self.store.open { self.store.open = false; self.track() }
        } as Any)

        store.changed = { [weak self] in self?.track() }
        loadPet(into: store)
        watch(into: store)
    }

    /// Over the notch of the built-in display when there is one, else centred on the main
    /// display's menu bar in a housing of the same size.
    func place() {
        // The first screen rather than the main one when none has a notch: `main` follows
        // the key window, and the housing would hop between displays with it.
        guard let screen = NSScreen.screens.first(where: { $0.safeAreaInsets.top > 0 }) ?? NSScreen.screens.first else { return }
        let f = screen.frame
        if screen.safeAreaInsets.top > 0, let l = screen.auxiliaryTopLeftArea, let r = screen.auxiliaryTopRightArea {
            geometry.notch = CGSize(width: f.width - l.width - r.width, height: screen.safeAreaInsets.top)
        } else {
            geometry.notch = CGSize(width: 185, height: max(24, f.maxY - screen.visibleFrame.maxY))
        }
        let host = NSHostingView(rootView: NotchView(s: store, g: geometry) { [weak self] t in self?.pick(t) })
        // The panel keeps the size given here. By default a hosting view resizes its window
        // to fit what it draws, which moved the notch off the camera as the ears grew.
        host.sizingOptions = []
        panel.contentView = host
        let w = Geometry.openWidth + 40, h: CGFloat = 520
        panel.setFrame(NSRect(x: f.midX - w / 2, y: f.maxY - h, width: w, height: h), display: true)
    }

    func shapeRect() -> NSRect {
        let size = geometry.size(store)
        let f = panel.frame
        return NSRect(x: f.midX - size.width / 2, y: f.maxY - size.height, width: size.width, height: size.height)
    }

    func inside() -> Bool { !store.rows.isEmpty && shapeRect().contains(NSEvent.mouseLocation) }

    /// Runs on every mouse move anywhere, so it only writes what changed.
    func track() {
        let over = inside()
        if panel.ignoresMouseEvents == over { panel.ignoresMouseEvents = !over }
        if store.hover != (over && !store.open) { store.hover = over && !store.open }
        // Leaving the open inbox folds it, the way a menu closes, after a beat so a pointer
        // that overshoots the edge and comes back does not lose it.
        if store.open, !over, !closing {
            closing = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { [weak self] in
                guard let self else { return }
                self.closing = false
                guard self.store.open, !self.inside() else { return }
                self.store.open = false
                self.track()
            }
        }
    }

    func toggle() {
        store.open.toggle()
        if store.open { loadPet(into: store) }
        track()
    }

    func pick(_ terminal: Int) {
        store.open = false
        track()
        jump(to: terminal)
    }
}

// One notch per person: a second copy, started by hand beside the one launchd keeps,
// leaves at once rather than drawing over it.
let lockPath = NSTemporaryDirectory() + "qoro-notch.lock"
let lock = open(lockPath, O_CREAT | O_RDWR, 0o600)
if lock < 0 || flock(lock, LOCK_EX | LOCK_NB) != 0 {
    exit(0)
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let delegate = App()
    app.delegate = delegate
    app.run()
}
