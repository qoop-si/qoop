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
import Combine
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

/// A stretch of one terminal row sharing a style, as `qoro read --json` prints it.
struct Run: Decodable {
    let t: String
    var fg: String? = nil
    var bg: String? = nil
    var b: Bool? = nil
    var u: Bool? = nil
    var v: Bool? = nil
    var d: Bool? = nil
    var s: Bool? = nil

    var plain: String { t }
}

struct Screen: Decodable {
    let alt: Bool
    let behind: Int
    let rows: [[Run]]
}

struct Back: Decodable {
    let from: Int
    let total: Int
    let rows: [[Run]]
}

struct Transcript: Decodable {
    struct Message: Decodable {
        let role: String
        let text: String
    }
    let messages: [Message]
}

@MainActor
final class Store: ObservableObject {
    @Published var rows: [Row] = []
    @Published var pixels: [[Color?]] = []
    @Published var open = false {
        didSet { if !open { expanded = nil } }
    }
    @Published var hover = false
    /// Bumped each time an agent finishes a turn, so the pet can cheer once.
    @Published var cheers = 0
    /// Told after every change of rows, so the panel re-reads where its shape now is
    /// without waiting for the mouse to move.
    var changed: (() -> Void)?
    /// The row whose terminal is open under it, and the live view of that terminal.
    @Published var expanded: Int? {
        didSet {
            guard expanded != oldValue else { return }
            mirror?.stop()
            // The reply line of the row that closed may have had the keyboard.
            release?()
            mirror = expanded.map { Mirror(terminal: $0) }
            mirror?.start()
            changed?()
        }
    }
    private(set) var mirror: Mirror?
    /// A line the island shows for a few seconds: what went wrong with the last action.
    @Published var note: String?
    /// Hands the keyboard back to whatever had it before the reply line.
    var release: (() -> Void)?

    func say(_ text: String) {
        note = text
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) { [weak self] in
            if self?.note == text { self?.note = nil }
        }
    }

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
        if let e = expanded, !rows.contains(where: { $0.terminal == e }) { expanded = nil }
        changed?()
    }
}

// MARK: qoro

/// Run qoro once: what it printed when it worked, else what it said went wrong.
func runQoroFull(_ args: [String]) -> Result<Data, QoroFailed> {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: qoro)
    p.arguments = args
    let out = Pipe(), err = Pipe()
    p.standardOutput = out
    p.standardError = err
    do { try p.run() } catch { return .failure(QoroFailed(why: "cannot run qoro")) }
    // Stderr on its own thread, so neither pipe can fill while the other is read.
    var said = Data()
    let drained = DispatchSemaphore(value: 0)
    DispatchQueue.global().async {
        said = err.fileHandleForReading.readDataToEndOfFile()
        drained.signal()
    }
    let printed = out.fileHandleForReading.readDataToEndOfFile()
    drained.wait()
    p.waitUntilExit()
    guard p.terminationStatus != 0 else { return .success(printed) }
    let text = String(decoding: said, as: UTF8.self)
        .replacingOccurrences(of: "qoro: ", with: "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
    return .failure(QoroFailed(why: text.isEmpty ? "qoro \(args.first ?? "") failed" : text))
}

struct QoroFailed: Error {
    let why: String
}

/// Run qoro once for its effect: nil when it worked, else what it said went wrong.
func runQoroChecked(_ args: [String]) -> String? {
    if case .failure(let f) = runQoroFull(args) { return f.why }
    return nil
}

/// Each line a running qoro prints, handed over as it arrives, until it exits.
func readLines(_ handle: FileHandle, _ line: (Data) -> Void) {
    var buffer = Data()
    while case let chunk = autoreleasepool(invoking: { handle.availableData }), !chunk.isEmpty {
        buffer.append(chunk)
        while let nl = buffer.firstIndex(of: 0x0a) {
            line(buffer[buffer.startIndex..<nl])
            buffer.removeSubrange(buffer.startIndex...nl)
        }
    }
}

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
    readLines(out.fileHandleForReading) { line in
        if let rows = try? JSONDecoder().decode([Row].self, from: line) {
            DispatchQueue.main.async { store.take(rows) }
        }
    }
    p.waitUntilExit()
}

/// A row the peek draws, with an identity that survives rows added above it: a history
/// row is its place in the terminal's history, a conversation row counts from the newest.
struct PeekRow: Identifiable {
    let id: Int
    let runs: [Run]
}

/// One terminal, seen from the notch: the same grid the interface draws, from the same
/// daemon, through `qoro read --watch`. Watching sends no size, so the agent's screen is
/// untouched. Above the screen, what came before it: the terminal's own history, or for
/// a program on the alternate screen (Claude Code, a full screen agent) its conversation,
/// which is the only record of what scrolled away.
@MainActor
final class Mirror: ObservableObject {
    let terminal: Int
    @Published var screen: [PeekRow] = []
    @Published var older: [PeekRow] = []
    @Published var more = true
    /// Bumped with every screen, so an answer pressed on one is not pressed again on the next.
    @Published var generation = 0
    /// Nil until the first screen says which kind of past there is.
    private var alt: Bool?
    private var loading = false
    private var extending = false
    private var messages = 0
    /// Rows behind the screen now; the first history row shown; the row just past the last.
    private var behind = 0
    private var oldest: Int?
    private var tail: Int?
    /// Bumped whenever what was read is thrown away, so a read from before is not kept.
    private var epoch = 0
    private var settle: DispatchWorkItem?
    private var process: Process?

    init(terminal: Int) { self.terminal = terminal }

    func start() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: qoro)
        p.arguments = ["read", String(terminal), "--watch", "--json"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return }
        process = p
        Thread.detachNewThread { [weak self] in
            autoreleasepool {
                readLines(out.fileHandleForReading) { line in
                    guard let screen = try? JSONDecoder().decode(Screen.self, from: line) else { return }
                    DispatchQueue.main.async { self?.show(screen) }
                }
            }
            p.waitUntilExit()
        }
    }

    func stop() {
        settle?.cancel()
        process?.terminate()
        process = nil
    }

    private func show(_ s: Screen) {
        generation += 1
        var rows = s.rows
        // A screen's blank bottom is room the program has not used, not something to read.
        while let last = rows.last, last.allSatisfy({ $0.t.trimmingCharacters(in: .whitespaces).isEmpty }) {
            rows.removeLast()
        }
        screen = rows.enumerated().map { PeekRow(id: 1 << 40 + $0.offset, runs: $0.element) }
        behind = s.behind
        if alt != s.alt {
            // The first screen, or the program went to or from the full screen: the past
            // above is another kind now, so it starts over, read at once.
            alt = s.alt
            restart()
            return
        }
        guard !s.alt, let t = tail else { return }
        if s.behind > t { extend(from: t) }
        // The history is written a beat after the screen moves, and the last screen of a
        // burst can still carry the count from before it: once output has been quiet for
        // longer than a write takes, the history is read once more against its own count.
        settle?.cancel()
        let item = DispatchWorkItem { [weak self] in
            guard let self, let t = self.tail else { return }
            self.extend(from: t, settling: true)
        }
        settle = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.7, execute: item)
    }

    /// Forget what was read above the screen and read its newest page again. Anything still
    /// on its way from before carries an older epoch and is dropped when it lands.
    private func restart() {
        epoch += 1
        older = []
        oldest = nil
        tail = nil
        messages = 0
        more = true
        loadOlder()
    }

    /// The next page of what came before, when the top of the view comes into sight.
    func loadOlder() {
        guard let alt, more, !loading else { return }
        loading = true
        let (t, have, total, end, era) = (terminal, messages, behind, oldest ?? behind, epoch)
        DispatchQueue.global().async { [weak self] in
            var rows: [PeekRow] = []
            var count = have
            var more = false
            var from: Int?
            // Where history ends as far as this read knows: the tail when nothing came back.
            var known: Int?
            if alt {
                // The conversation is read newest first and cut; asking for more is asking
                // for a longer tail, bounded so one agent cannot fill the notch's memory.
                let want = min(have + 20, 200)
                if let data = runQoro(["history", String(t), "--last", String(want), "--json"]),
                   let tr = try? JSONDecoder().decode(Transcript.self, from: data) {
                    let lines = conversation(tr.messages)
                    rows = lines.enumerated().map { PeekRow(id: $0.offset - lines.count, runs: $0.element) }
                    count = tr.messages.count
                    more = tr.messages.count >= want && want < 200
                } else {
                    more = true
                }
            } else if end > 0 {
                let start = max(0, end - 200)
                if let back = readBack(t, up: total - start, rows: end - start) {
                    // Cut on the rows' own places: history written since `total` was read
                    // moves the page newer, and a trimmed history pins it to the oldest row.
                    rows = back.rows.enumerated()
                        .filter { back.from + $0.offset < end }
                        .map { PeekRow(id: back.from + $0.offset, runs: $0.element) }
                    from = back.from
                    known = min(end, back.total)
                    more = back.from > 0 && back.from < end
                } else {
                    // Not read this time; the top of the view asks again.
                    more = true
                }
            } else {
                known = 0
            }
            DispatchQueue.main.async {
                guard let self else { return }
                self.loading = false
                guard self.epoch == era else {
                    self.loadOlder()
                    return
                }
                if alt {
                    self.older = rows
                } else {
                    self.older = rows + self.older
                    if let from, !rows.isEmpty { self.oldest = from }
                    // Where the rows read end, not where the screen said they would; with
                    // none read, where the history ends, so what scrolls off later is kept.
                    if self.tail == nil, let end = rows.last.map({ $0.id + 1 }) ?? known {
                        self.tail = end
                        if end < self.behind { self.extend(from: end) }
                    }
                }
                self.messages = count
                self.more = more
            }
        }
    }

    /// Read what went behind the screen since `t` and put it under what was read.
    private func extend(from t: Int, tries: Int = 0, settling: Bool = false) {
        guard !extending else { return }
        let (term, now, era) = (terminal, behind, epoch)
        // Far behind is not worth reading row by row: start over from the newest page.
        if now - t > 1000 {
            restart()
            return
        }
        extending = true
        DispatchQueue.global().async { [weak self] in
            // Ahead of the screen's count, or level with it when settling: the newest rows
            // are asked for, and only those past `t` are kept.
            var back = now > t ? readBack(term, up: now - t, rows: now - t) : readBack(term, up: 500, rows: 500)
            // The history is written a beat after the screen says rows went behind it, so
            // it may count fewer: asked again against its own count.
            if let b = back, b.from + b.rows.count <= t, b.total > t {
                back = readBack(term, up: b.total - t, rows: min(b.total - t, 1000))
            }
            DispatchQueue.main.async {
                guard let self else { return }
                self.extending = false
                guard let back, self.epoch == era, self.tail == t else { return }
                let rows = back.rows.enumerated()
                    .filter { back.from + $0.offset >= t }
                    .map { PeekRow(id: back.from + $0.offset, runs: $0.element) }
                self.older += rows
                self.tail = (rows.last?.id ?? (t - 1)) + 1
                self.bound()
                // Still short of what the screen or the history counts, and nothing else
                // will ask: this does, a few times, once the history has caught up.
                let target = max(self.behind, back.total)
                if let tail = self.tail, tail < target, tries < 5, !settling || !rows.isEmpty {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [weak self] in
                        guard let self, self.epoch == era, let tail = self.tail else { return }
                        self.extend(from: tail, tries: rows.isEmpty ? tries + 1 : 0)
                    }
                }
            }
        }
    }

    /// A terminal that keeps printing while its peek stays open grows the history above it
    /// without end; past this many rows the oldest go, and can be read again from the top.
    private func bound() {
        let cap = 2000
        guard older.count > cap else { return }
        older.removeFirst(older.count - cap)
        oldest = older.first?.id
        more = true
    }
}

/// History rows from `qoro read --back`, or nil when it could not be read.
func readBack(_ terminal: Int, up: Int, rows: Int) -> Back? {
    guard rows > 0,
          let data = runQoro(["read", String(terminal), "--back", String(up), "--lines", String(rows), "--json"]) else { return nil }
    return try? JSONDecoder().decode(Back.self, from: data)
}

/// An agent's conversation as terminal rows: what the person said marked, tool calls
/// dim, each message cut to a screenful.
func conversation(_ messages: [Transcript.Message]) -> [[Run]] {
    var out: [[Run]] = []
    for m in messages {
        let lines = m.text.split(separator: "\n", omittingEmptySubsequences: false).prefix(24)
        for (i, line) in lines.enumerated() {
            let text = String(line)
            switch m.role {
            case "user":
                out.append([Run(t: i == 0 ? "\u{203a} " : "  ", fg: "#d77757", b: true), Run(t: text, fg: "#ffffff")])
            default:
                out.append([Run(t: "  " + text, fg: text.hasPrefix("Bash ") ? "#8a8a8a" : "#d0d0d0")])
            }
        }
        out.append([])
    }
    return out
}

/// Switch the interface to a terminal, then raise the app that interface runs in: the
/// first process up its parent chain that is an app the window server knows.
func jump(to terminal: Int, store: Store) {
    DispatchQueue.global().async {
        let pids: [Int]
        switch runQoroFull(["focus", String(terminal), "--json"]) {
        case .success(let data):
            let reply = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            pids = reply?["clients"] as? [Int] ?? []
        case .failure(let f):
            // Said rather than doing nothing: most often a session started before this
            // qoro, whose daemon does not know the request yet.
            DispatchQueue.main.async {
                store.say(f.why.contains("older") ? "restart qoro to jump there: qoro stop, then qoro" : f.why)
            }
            return
        }
        if pids.isEmpty {
            DispatchQueue.main.async { store.say("no qoro window is open to show it in") }
            return
        }
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
    let open: Bool
    @State private var over = false

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(.white.opacity(0.45))
                .rotationEffect(.degrees(open ? 90 : 0))
            Text(row.label)
                .fontWeight(.medium)
                .foregroundStyle(row.kind == .waiting ? amber : row.kind == .idle ? .white.opacity(0.6) : .white)
            Text([row.place, row.detail].filter { !$0.isEmpty }.joined(separator: " \u{00b7} "))
                .foregroundStyle(.white.opacity(0.42))
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 8)
            Text("\(row.terminal)")
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.white.opacity(0.4))
        }
        .font(.system(size: 13))
        .padding(.horizontal, 8)
        .frame(height: 30)
        .background(RoundedRectangle(cornerRadius: 8).fill(
            row.kind == .waiting ? amber.opacity(over || open ? 0.18 : 0.1) : .white.opacity(over || open ? 0.08 : 0)))
        .contentShape(Rectangle())
        .onHover { over = $0 }
    }
}

/// One terminal row drawn in its own colours.
func line(_ runs: [Run]) -> Text {
    var out = AttributedString(runs.isEmpty ? " " : "")
    for r in runs {
        var piece = AttributedString(r.t)
        var fg = r.fg.flatMap(hex) ?? Color(white: 0.86)
        var bg = r.bg.flatMap(hex)
        if r.v == true {
            let was = fg
            fg = bg ?? Color(white: 0.05)
            bg = was
        }
        piece.foregroundColor = r.d == true ? fg.opacity(0.55) : fg
        if let bg { piece.backgroundColor = bg }
        if r.b == true { piece.font = .system(size: 10, weight: .bold, design: .monospaced) }
        if r.u == true { piece.underlineStyle = .single }
        if r.s == true { piece.strikethroughStyle = .single }
        out += piece
    }
    return Text(out)
}

/// A row as the peek draws it: one line at its full width, never cut short with an
/// ellipsis, since the view scrolls sideways to what is past the edge.
struct TermLine: View {
    let runs: [Run]
    var body: some View {
        line(runs).lineLimit(1).fixedSize(horizontal: true, vertical: false)
    }
}

struct Option {
    let key: String
    let label: String
    /// The one the agent's own cursor is on, which Enter would pick.
    let current: Bool
}

/// A numbered menu on the bottom of a screen, the way agents ask: `❯ 1. Yes`, `2. No`.
/// Only the lowest run of numbered rows counts: a numbered plan printed above the question
/// is not its answers, and its labels on the buttons would be a lie about what is sent.
func menu(_ screen: [[Run]]) -> [Option] {
    func option(_ row: [Run]) -> Option? {
        var text = Substring(row.map(\.plain).joined())
        // Inside a box the border comes first.
        text = text.drop { $0 == " " || $0 == "\u{2502}" }
        let current = text.first == "\u{276f}" || text.first == ">"
        if current { text = text.dropFirst().drop { $0 == " " } }
        guard let digit = text.first, ("1"..."9").contains(digit), text.dropFirst().hasPrefix(". ") else { return nil }
        let label = text.dropFirst(3).trimmingCharacters(in: CharacterSet(charactersIn: " \u{2502}"))
        return label.isEmpty ? nil : Option(key: String(digit), label: String(label.prefix(26)), current: current)
    }
    var block: [Option] = []
    for row in screen.suffix(16).reversed() {
        if let o = option(row) {
            block.insert(o, at: 0)
        } else if !block.isEmpty {
            // The first row above the block that is not an option (the question, a blank)
            // ends it.
            break
        }
    }
    let keys = block.map(\.key)
    // A menu has the agent's own cursor on one of its options; a numbered list does not.
    return block.count >= 2 && block.contains(where: \.current) && Set(keys).count == keys.count ? block : []
}

/// The terminal of an open row: live, scrollable back into what came before, with a way
/// to answer it and a way to go to it.
struct TermPeek: View {
    static let height: CGFloat = 170
    @ObservedObject var m: Mirror
    let row: Row
    @ObservedObject var store: Store
    let open: (Int) -> Void
    @State private var reply = ""
    /// The menu an option was pressed on: nothing more is pressed until another menu shows,
    /// so a second click cannot answer a question that has not been seen yet.
    @State private var pressedOn: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ScrollView([.vertical, .horizontal], showsIndicators: false) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if m.more {
                        Text("earlier")
                            .foregroundStyle(.white.opacity(0.3))
                            .onAppear { m.loadOlder() }
                    }
                    ForEach(m.older) { TermLine(runs: $0.runs) }
                    if !m.older.isEmpty {
                        Rectangle().fill(.white.opacity(0.12)).frame(height: 1).padding(.vertical, 4)
                    }
                    ForEach(m.screen) { TermLine(runs: $0.runs) }
                }
                .font(.system(size: 10, design: .monospaced))
                .padding(8)
            }
            .defaultScrollAnchor(.bottomLeading)
            .frame(height: TermPeek.height)
            .background(RoundedRectangle(cornerRadius: 10).fill(Color(white: 0.05)))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(.white.opacity(0.08)))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            HStack(spacing: 8) {
                let options = row.state == "blocked" ? menu(m.screen.map(\.runs)) : []
                let shown = options.map { $0.key + $0.label }.joined(separator: "\n")
                let locked = pressedOn == shown
                if options.isEmpty {
                    TextField("Type to terminal \(row.terminal)", text: $reply)
                        .textFieldStyle(.plain)
                        .font(.system(size: 12.5))
                        .padding(.horizontal, 12)
                        .frame(height: 28)
                        .background(Capsule().fill(.white.opacity(0.08)))
                        .onSubmit { send() }
                } else {
                    ForEach(options, id: \.key) { o in
                        Button { press(o.key, on: shown) } label: {
                            Text("\(o.key) \(o.label)").lineLimit(1)
                        }
                        .buttonStyle(.plain)
                        .disabled(locked)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(o.current ? .black : .white)
                        .padding(.horizontal, 11).frame(height: 28)
                        .background(Capsule().fill(o.current ? amber : .white.opacity(0.1)))
                        .opacity(locked ? 0.4 : 1)
                    }
                    Spacer(minLength: 0)
                }
                Button { open(row.terminal) } label: {
                    Label("open in qoro", systemImage: "arrow.up.right")
                        .labelStyle(.titleAndIcon)
                }
                .buttonStyle(.plain)
                .font(.system(size: 11.5))
                .foregroundStyle(.white.opacity(0.55))
                .fixedSize()
            }
        }
    }

    private func send() {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        reply = ""
        let (t, store) = (row.terminal, store)
        // Always a prompt, which qoro refuses while the agent is blocked: typed text and an
        // Enter into a menu it could not read would pick whatever option is highlighted.
        let giveBack: (String) -> Void = { why in
            store.say(why)
            if reply.isEmpty { reply = text }
        }
        DispatchQueue.global().async {
            if let why = runQoroChecked(["prompt", String(t), "--", text]) {
                DispatchQueue.main.async { giveBack(why) }
            } else {
                // Sent: the keyboard goes back to where the person was, so what they type
                // next does not land in this line and go to this agent.
                DispatchQueue.main.async { if reply.isEmpty { store.release?() } }
            }
        }
    }

    private func press(_ key: String, on shown: String) {
        pressedOn = shown
        let (t, store) = (row.terminal, store)
        let failed: (String) -> Void = { why in
            store.say(why)
            // Not sent, so the same menu may be answered again.
            if pressedOn == shown { pressedOn = nil }
        }
        DispatchQueue.global().async {
            if let why = runQoroChecked(["send", String(t), key]) {
                DispatchQueue.main.async { failed(why) }
            }
        }
    }
}

struct Island: View {
    @ObservedObject var s: Store
    let open: (Int) -> Void

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
            if let note = s.note {
                Text(note)
                    .font(.system(size: 12))
                    .foregroundStyle(amber)
                    .lineLimit(2)
                    .padding(.top, 6)
                    .transition(.opacity)
            }
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
                let isOpen = s.expanded == row.terminal
                RowView(row: row, open: isOpen)
                    .padding(.horizontal, -8)
                    .onTapGesture { withAnimation(spring) { s.expanded = isOpen ? nil : row.terminal } }
                    .accessibilityElement(children: .combine)
                    .accessibilityAddTraits(.isButton)
                    .accessibilityHint(isOpen ? "Hides terminal \(row.terminal)" : "Shows terminal \(row.terminal) here")
                    .accessibilityAction { s.expanded = isOpen ? nil : row.terminal }
                if isOpen, let mirror = s.mirror {
                    TermPeek(m: mirror, row: row, store: s, open: open)
                        .padding(.top, 4).padding(.bottom, 8)
                        .transition(.opacity.combined(with: .offset(y: -8)))
                }
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
    /// The tallest the inbox grows; past it the list scrolls. Inside the panel's 700.
    static let openMaxHeight: CGFloat = 640
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
        let peek: CGFloat = s.expanded == nil ? 0 : TermPeek.height + 8 + 28 + 12
        let note: CGFloat = s.note == nil ? 0 : 26
        let full = notch.height + 40 + CGFloat(sections) * 25 + CGFloat(s.rows.count) * Geometry.rowHeight + 4 + peek + note
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
    let open: (Int) -> Void

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
                    Island(s: s, open: open)
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
        .animation(spring, value: s.expanded)
        .animation(spring, value: s.note)
    }
}

// MARK: panel

final class NotchPanel: NSPanel {
    // Key only so the reply line can take typing; being non-activating, it never takes
    // the app to the front or the focus from the terminal under it until clicked in.
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class App: NSObject, NSApplicationDelegate {
    let store = Store()
    var panel: NotchPanel!
    var geometry = Geometry(notch: CGSize(width: 185, height: 32))
    var monitors: [Any] = []
    var closing = false
    var folded: AnyCancellable?

    func applicationDidFinishLaunching(_ note: Notification) {
        panel = NotchPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.level = .statusBar + 1
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        panel.ignoresMouseEvents = true
        // Key only when a control that types asks for it, the reply line; a click on a
        // row or the notch leaves the keyboard where it was.
        panel.becomesKeyOnlyIfNeeded = true
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
        // Folding hands the keyboard back: a panel left key after the reply line is gone
        // would swallow what the person types next into the app under it.
        store.release = { [weak self] in self?.release() }
        folded = store.$open.sink { [weak self] open in
            if !open { self?.release() }
        }
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
        let host = NSHostingView(rootView: NotchView(s: store, g: geometry) { [weak self] t in self?.go(t) })
        // The panel keeps the size given here. By default a hosting view resizes its window
        // to fit what it draws, which moved the notch off the camera as the ears grew.
        host.sizingOptions = []
        panel.contentView = host
        let w = Geometry.openWidth + 40, h: CGFloat = 700
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
        // Not while typing: the reply line holds the keyboard, and a pointer drifting off the
        // edge must not take the half written answer with it.
        if store.open, !over, !closing, !(panel.isKeyWindow && panel.firstResponder is NSText) {
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

    func release() {
        panel.makeFirstResponder(nil)
        guard panel.isKeyWindow else { return }
        panel.orderOut(nil)
        panel.orderFrontRegardless()
    }

    func go(_ terminal: Int) {
        jump(to: terminal, store: store)
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
