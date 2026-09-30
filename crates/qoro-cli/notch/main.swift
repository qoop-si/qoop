// The notch: qoro's agents at the top of the screen.
//
// Closed, it is the camera housing grown two ears: the person's pet on the left with
// a badge counting the agents that need them, and on the right how many are
// working and idle. Clicked, it drops an inbox of every agent, grouped by what it is
// doing; clicking a row switches the qoro interface to that terminal and brings its
// window forward. With no qoro running, or no agents in it, the notch is left alone.
//
// Everything it knows comes from the `qoro` binary that started it, named as the
// first argument by `qoro notch run`: `agents --watch` for the rows, `avatar show
// --json` for the pet, `focus` for a click. It links no qoro code and reads no qoro
// file, so the command line stays the one contract. It shows every running session:
// each command about a row names that row's session.

import AppKit
import Combine
import SwiftUI

let qoro = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "qoro"

// MARK: model

/// A terminal of one session: numbers repeat across sessions, the pair does not.
struct Tab: Hashable {
    let session: String
    let terminal: Int

    /// A qoro command about this terminal, run in its session.
    func qoro(_ rest: String...) -> [String] { ["--session", session] + rest }
}

struct Row: Decodable, Equatable, Identifiable {
    let session: String
    let terminal: Int
    let name: String?
    /// Null while a terminal is still starting, which must not drop the whole line.
    let program: String?
    let state: String
    let agent: Bool
    let agent_title: String?
    let cwd: String?
    /// The space it belongs to (`qoro agents --json`, `AgentRow.workspace`), so its rows
    /// can carry the same rail the sidebar draws down a space's left edge.
    let workspace: Int64

    var tab: Tab { Tab(session: session, terminal: terminal) }
    var id: Tab { tab }

    /// Set when the qoro interface shows this row as done: it finished while the person
    /// was on another tab. Absent from a qoro that predates it.
    let news: Bool?

    enum Kind { case waiting, done, working, idle }

    var kind: Kind {
        switch state {
        case "blocked", "failed": return .waiting
        case "working": return .working
        default: return news == true ? .done : .idle
        }
    }

    /// The row's state as one of the sidebar's six pictures (`DotMark.Kind`): waiting
    /// splits into wants (blocked) or failed, by the row's own state rather than the
    /// section's worst.
    var dotKind: DotMark.Kind {
        switch kind {
        case .working: return .working
        case .waiting: return state == "failed" ? .failed : .wants
        case .done: return .done
        case .idle: return .idle
        }
    }

    var label: String { name ?? program ?? "terminal" }
    var place: String { cwd.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "" }
    var detail: String {
        if state == "failed" { return "failed" }
        return agent_title ?? (kind == .waiting ? "needs you" : "")
    }
    /// Blocked or Failed for a row that needs you, its state's green once done, nothing
    /// for the rest (working keeps plain text; the mark carries its colour instead).
    var ink: Color? {
        switch kind {
        case .waiting: return DotMark.ink(dotKind)
        case .done: return DotMark.ink(.done)
        default: return nil
        }
    }
}

struct Pet: Decodable {
    let pixels: [[String?]]
}

/// A stretch of one terminal row sharing a style, as `qoro read --json` prints it.
struct Run: Decodable {
    var t: String
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
    @Published var expanded: Tab? {
        didSet {
            guard expanded != oldValue else { return }
            mirror?.stop()
            // The peek of the row that closed may have had the keyboard.
            release?()
            mirror = expanded.map { Mirror(tab: $0) }
            mirror?.start()
            changed?()
        }
    }
    private(set) var mirror: Mirror?
    /// A line the island shows for a few seconds: what went wrong with the last action.
    @Published var note: String?
    /// Takes the keyboard for a peek being typed into, and hands it back after.
    var takeKeyboard: (() -> Void)?
    /// The terminal a peek is being typed into, and each terminal's queue of keys: kept
    /// here rather than in the peek, which is built again whenever its row moves.
    var typingTo: Tab?
    private var queues: [Tab: Keys] = [:]

    func keys(for tab: Tab) -> Keys {
        if let k = queues[tab] { return k }
        let k = Keys(tab: tab, store: self)
        queues[tab] = k
        return k
    }
    var release: (() -> Void)?

    func say(_ text: String) {
        note = text
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) { [weak self] in
            if self?.note == text { self?.note = nil }
        }
    }

    var waiting: Int { rows.filter { $0.kind == .waiting }.count }
    /// The worst state that needs you, as the TUI ranks them: blocked, then failed.
    var needKind: DotMark.Kind {
        rows.contains { $0.state == "failed" } && !rows.contains { $0.state == "blocked" } ? .failed : .wants
    }
    var needInk: Color { DotMark.ink(needKind) }
    var working: Int { rows.filter { $0.kind == .working }.count }
    var idle: Int { rows.filter { $0.kind == .idle }.count }
    /// Finished while the person was elsewhere, as the qoro interface shows it.
    var finished: Int { rows.filter { $0.kind == .done }.count }
    /// Whether the rows come from more than one session, so each says which.
    var mixed: Bool { Set(rows.map(\.session)).count > 1 }

    func take(_ next: [Row]) {
        let before = Dictionary(rows.map { ($0.tab, $0.kind) }, uniquingKeysWith: { a, _ in a })
        let finished = next.contains { before[$0.tab] == .working && ($0.kind == .idle || $0.kind == .done) }
        // A slept terminal is a remembered row with no process behind it: nothing to
        // count, and nothing a click could show.
        let agents = next.filter { $0.agent && $0.state != "sleeping" }
        if agents != rows { rows = agents }
        if finished { cheers += 1 }
        if rows.isEmpty { open = false }
        if let e = expanded, !rows.contains(where: { $0.tab == e }) { expanded = nil }
        changed?()
    }
}

// MARK: qoro

/// Run qoro once: what it printed when it worked, else what it said went wrong. `what`
/// names the command, for the rare refusal that printed nothing to say: every command
/// about a row starts with `--session`, so the first argument is no use as a name.
func runQoroFull(_ args: [String], _ what: String) -> Result<Data, QoroFailed> {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: qoro)
    p.arguments = args
    let out = Pipe(), err = Pipe()
    p.standardOutput = out
    p.standardError = err
    do { try p.run() } catch { return .failure(QoroFailed(why: Refusal.noQoro)) }
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
    let text = qoroSaid(said)
    return .failure(QoroFailed(why: text.isEmpty ? "\(what) failed" : text))
}

/// What qoro said, said for a person: its own sentence with the `qoro: ` that starts
/// every line of it taken off, and nothing of its own when it said nothing.
func qoroSaid(_ raw: Data) -> String {
    String(decoding: raw, as: UTF8.self)
        .replacingOccurrences(of: "qoro: ", with: "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

struct QoroFailed: Error {
    let why: String
}

/// The sentences the notch writes itself, for what qoro left unsaid. `noQoro` is a qoro
/// that would not start at all. The other two are a read qoro answered and this build
/// could not: the qoro behind the notch is newer than the notch, which is what a notch
/// left running across an update is, and `qoro update` brings the notch up with it.
enum Refusal {
    static let noQoro = "cannot run qoro"
    static let screen = "not a screen this notch can read: qoro update brings the notch up with it"
    static let page = "not a page this notch can read: qoro update brings the notch up with it"
}

/// Run qoro once for its effect: nil when it worked, else what it said went wrong.
func runQoroChecked(_ args: [String], _ what: String) -> String? {
    if case .failure(let f) = runQoroFull(args, what) { return f.why }
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

/// Follow `qoro agents --watch --all --json`: one JSON array per line of every running
/// session's rows, a line each time anything a row shows changes. It hears sessions start
/// and end itself; should it ever exit, it is started again a few seconds later.
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
    p.arguments = ["agents", "--watch", "--all", "--json"]
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
    let tab: Tab
    @Published var screen: [PeekRow] = []
    @Published var older: [PeekRow] = []
    @Published var more = true
    /// What qoro would not read, said in place of the rows it would not read: a terminal
    /// that cannot be read and a terminal with nothing in it look the same otherwise, and
    /// only one of them is a bug. The screen and the past are read by different commands
    /// and each clears only its own, so a live screen cannot wipe a refused page.
    @Published var screenRefusal: String?
    @Published var pageRefusal: String?
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
    /// Set when the row closed, so a read still on its way says nothing about a peek that
    /// is folding away, and the watcher this app stopped says nothing at all.
    private var stopped = false

    init(tab: Tab) { self.tab = tab }

    func start() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: qoro)
        p.arguments = tab.qoro("read", String(tab.terminal), "--watch", "--json")
        let out = Pipe(), err = Pipe()
        p.standardOutput = out
        p.standardError = err
        guard (try? p.run()) != nil else {
            screenRefusal = Refusal.noQoro
            return
        }
        process = p
        // Stderr on its own thread, as in runQoroFull, so a qoro that refuses with a long
        // sentence and a screen that never comes cannot fill one pipe while the other is
        // read. A thread of its own rather than a task on the shared queue: this one lives
        // as long as the row is open, and that queue is where the pet, a click to focus and
        // a send run as well.
        var said = Data()
        let drained = DispatchSemaphore(value: 0)
        Thread.detachNewThread {
            said = err.fileHandleForReading.readDataToEndOfFile()
            drained.signal()
        }
        Thread.detachNewThread { [weak self] in
            readLines(out.fileHandleForReading) { line in
                // A pool per line, not per watch: a peek can stay open for hours, and every
                // screen decoded is Foundation objects that would wait for the child to end.
                autoreleasepool {
                    // A line with nothing on it is not a screen this build cannot read.
                    guard !line.allSatisfy({ $0 == 0x20 || $0 == 0x09 || $0 == 0x0d }) else { return }
                    guard let screen = try? JSONDecoder().decode(Screen.self, from: line) else {
                        DispatchQueue.main.async { self?.refuse(Refusal.screen) }
                        return
                    }
                    DispatchQueue.main.async { self?.show(screen) }
                }
            }
            drained.wait()
            p.waitUntilExit()
            let why = qoroSaid(said)
            // A qoro that failed is worth a sentence whether or not screens came first: a
            // watch that dies on a dropped link leaves the last screen on the box for as long
            // as the row is listed. A child that ended without failing has nothing to explain,
            // its terminal is what ended, and the row goes with it. The hop is from this one
            // thread, so the last screen lands before the sentence that replaces it.
            guard p.terminationStatus != 0 else { return }
            DispatchQueue.main.async {
                guard let self, !self.stopped else { return }
                self.screenRefusal = why.isEmpty ? Refusal.screen : why
            }
        }
    }

    /// Say a screen this build could not read, once, and stop saying it while screens come.
    private func refuse(_ why: String) {
        if screenRefusal == nil { screenRefusal = why }
    }

    func stop() {
        stopped = true
        settle?.cancel()
        process?.terminate()
        process = nil
    }

    private func show(_ s: Screen) {
        // A screen already on its way when the row closed would otherwise ask for a page
        // of history this app no longer draws.
        guard !stopped else { return }
        generation += 1
        var rows = s.rows
        // A screen's blank bottom is room the program has not used, not something to read.
        while let last = rows.last, last.allSatisfy({ $0.t.trimmingCharacters(in: .whitespaces).isEmpty }) {
            rows.removeLast()
        }
        screen = rows.enumerated().map { PeekRow(id: 1 << 40 + $0.offset, runs: $0.element) }
        if screenRefusal != nil { screenRefusal = nil }
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
    /// on its way from before carries an older epoch and is dropped when it lands. Only the
    /// past starts over here: `show` calls this when the program changes screen, and what
    /// the watch itself was refused is still true.
    private func restart() {
        epoch += 1
        older = []
        oldest = nil
        tail = nil
        messages = 0
        more = true
        pageRefusal = nil
        loadOlder()
    }

    /// The next page of what came before, when the top of the view comes into sight.
    func loadOlder() {
        guard let alt, more, !loading else { return }
        loading = true
        let (t, have, total, end, era) = (tab, messages, behind, oldest ?? behind, epoch)
        DispatchQueue.global().async { [weak self] in
            var rows: [PeekRow] = []
            var count = have
            var more = false
            var from: Int?
            // Why the page could not be read, when it could not: the peek draws it in place
            // of the page, so a terminal whose past is unreadable is not an empty one.
            var refusal: String?
            // Where history ends as far as this read knows: the tail when nothing came back.
            var known: Int?
            if alt {
                // The conversation is read newest first and cut; asking for more is asking
                // for a longer tail, bounded so one agent cannot fill the notch's memory.
                let want = min(have + 20, 200)
                switch runQoroFull(t.qoro("history", String(t.terminal), "--last", String(want), "--json"), "qoro history") {
                case .success(let data):
                    if let tr = try? JSONDecoder().decode(Transcript.self, from: data) {
                        let lines = conversation(tr.messages)
                        rows = lines.enumerated().map { PeekRow(id: $0.offset - lines.count, runs: $0.element) }
                        count = tr.messages.count
                        more = tr.messages.count >= want && want < 200
                    } else {
                        // An old notch against a newer qoro: the answer is there and unreadable.
                        refusal = Refusal.page
                        more = true
                    }
                case .failure(let f):
                    refusal = f.why
                    // Not read this time; the top of the view asks again. What is drawn
                    // stays drawn, so a page that will not read does not blank the peek.
                    more = true
                }
            } else if end > 0 {
                let start = max(0, end - 200)
                switch readBack(t, up: total - start, rows: end - start) {
                case .success(let back):
                    // Cut on the rows' own places: history written since `total` was read
                    // moves the page newer, and a trimmed history pins it to the oldest row.
                    rows = back.rows.enumerated()
                        .filter { back.from + $0.offset < end }
                        .map { PeekRow(id: back.from + $0.offset, runs: $0.element) }
                    from = back.from
                    known = min(end, back.total)
                    more = back.from > 0 && back.from < end
                case .failure(let f):
                    refusal = f.why
                    // Not read this time; the top of the view asks again.
                    more = true
                }
            } else {
                known = 0
            }
            DispatchQueue.main.async {
                guard let self else { return }
                self.loading = false
                guard !self.stopped else { return }
                guard self.epoch == era else {
                    self.loadOlder()
                    return
                }
                if let refusal {
                    // Nothing read this time, so nothing is replaced: what is on screen is
                    // what was read last, and the sentence stands in for the page.
                    self.pageRefusal = refusal
                    self.more = more
                    return
                }
                if self.pageRefusal != nil { self.pageRefusal = nil }
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
        let (term, now, era) = (tab, behind, epoch)
        // Far behind is not worth reading row by row: start over from the newest page.
        if now - t > 1000 {
            restart()
            return
        }
        extending = true
        DispatchQueue.global().async { [weak self] in
            // Ahead of the screen's count, or level with it when settling: the newest rows
            // are asked for, and only those past `t` are kept.
            var back = now > t ? readBack(term, up: now - t, rows: now - t)
                               : readBack(term, up: 500, rows: 500)
            // The history is written a beat after the screen says rows went behind it, so
            // it may count fewer: asked again against its own count.
            if case .success(let b) = back, b.from + b.rows.count <= t, b.total > t {
                back = readBack(term, up: b.total - t, rows: min(b.total - t, 1000))
            }
            DispatchQueue.main.async {
                guard let self else { return }
                self.extending = false
                guard !self.stopped, self.epoch == era, self.tail == t else { return }
                guard case .success(let back) = back else {
                    // What went behind the screen could not be read: said where those rows
                    // would be, and the screen keeps arriving over it.
                    if case .failure(let f) = back { self.pageRefusal = f.why }
                    return
                }
                if self.pageRefusal != nil { self.pageRefusal = nil }
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

/// History rows from `qoro read --back`, or what it said would not come.
func readBack(_ tab: Tab, up: Int, rows: Int) -> Result<Back, QoroFailed> {
    let args = tab.qoro("read", String(tab.terminal), "--back", String(up), "--lines", String(rows), "--json")
    switch runQoroFull(args, "qoro read") {
    case .failure(let f): return .failure(f)
    case .success(let data):
        guard let back = try? JSONDecoder().decode(Back.self, from: data) else {
            return .failure(QoroFailed(why: Refusal.page))
        }
        return .success(back)
    }
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
func jump(to tab: Tab, store: Store) {
    DispatchQueue.global().async {
        let pids: [Int]
        switch runQoroFull(tab.qoro("focus", String(tab.terminal), "--json"), "qoro focus") {
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

/// The product's dot language: the same inks, marks and logo geometry as the terminal
/// sidebar (`crates/qoro-tui/src/theme.rs`, `crates/qoro-tui/src/sidebar.rs`) and
/// `docs/brand.md`, so an agent's state, a space's identity or the logo reads the same
/// wherever it is drawn.
enum Palette {
    static let working = hex("#00afff")!
    static let wants = hex("#ff8700")!
    static let failed = hex("#ff00ff")!
    static let done = hex("#00af5f")!
    static let idle = hex("#6c6c6c")!
    static let asleep = hex("#af87d7")!
    /// The selected tab's ink, and a peek being typed into: not a state, kept alongside
    /// the rest since it was one of the four globals this enum replaces.
    static let focus = hex("#ff87d7")!
    static let ember = hex("#fd4b03")!
    /// `ember` reads 2.9:1 on the settings window's light background, under the 3:1 a
    /// mark needs; this is the same hue a step darker, 4.7:1 there.
    static let emberOnLight = hex("#c23e00")!
    static let cream = hex("#fcf7f1")!
    static let ground = hex("#0f0f11")!
    /// The identity palette, dark: violet, lime, cyan, pink, sky, yellow, the hex of the
    /// sidebar's `SPACE_INKS` (177 191 87 218 117 228). The notch is always a dark ground.
    private static let spaceHex = ["#d787ff", "#d7ff5f", "#5fffff", "#ffafd7", "#87d7ff", "#ffff87"]
    static let spaces: [Color] = spaceHex.map { hex($0)! }

    /// A space's ink by id, the sidebar's `space_of`: `abs(id) % 6`. One daemon per
    /// machine (since bdb5544) makes a workspace id unique here too.
    static func space(_ id: Int64) -> Color { spaces[Int(id.magnitude % UInt64(spaces.count))] }
}

// Kept as the names the rest of the file already reads them by.
let blocked = Palette.wants
let failed = Palette.failed
let done = Palette.done
let focus = Palette.focus
let spring = Animation.spring(response: 0.42, dampingFraction: 0.74)

/// An agent's state as the sidebar's nine-dot picture (`cell_mark`/`breath` in
/// `crates/qoro-tui/src/sidebar.rs`, pictures in `docs/brand.md`): lit dots in the
/// state's ink, unlit dots nothing, the same grid everywhere an agent's state is drawn.
struct DotMark: View {
    enum Kind { case working, wants, failed, done, idle, asleep }

    let kind: Kind
    let size: CGFloat
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    static func ink(_ kind: Kind) -> Color {
        switch kind {
        case .working: return Palette.working
        case .wants: return Palette.wants
        case .failed: return Palette.failed
        case .done: return Palette.done
        case .idle: return Palette.idle
        case .asleep: return Palette.asleep
        }
    }

    /// The empty ring: wants, and the logo's letters.
    static let ring: [(Int, Int)] = [(0, 0), (1, 0), (2, 0), (2, 1), (2, 2), (1, 2), (0, 2), (0, 1)]

    /// The centre and its arms at breath `frame`, ported from the sidebar's `breath`: arm
    /// order top, right, bottom, left; a breath's eight steps deepen 1,1,2,3,4,4,3,2; the
    /// next breath starts one arm further clockwise, so the loop is thirty two frames.
    private static let arm: [(Int, Int)] = [(1, 0), (2, 1), (1, 2), (0, 1)]
    /// The still picture, under reduce motion: centre, top, right.
    private static let still: [(Int, Int)] = [(1, 1), arm[0], arm[1]]

    private static func breath(_ frame: Int) -> [(Int, Int)] {
        let depth = [1, 1, 2, 3, 4, 4, 3, 2]
        let base = frame / 8
        var dots = [(1, 1)]
        for i in 0..<depth[frame % 8] { dots.append(DotMark.arm[(base + i) % 4]) }
        return dots
    }

    private var dots: [(Int, Int)] {
        switch kind {
        case .working: return DotMark.still
        case .wants: return DotMark.ring
        case .failed: return [(0, 0), (2, 0), (1, 1), (0, 2), (2, 2)]
        case .done: return (0..<3).flatMap { y in (0..<3).map { x in (x, y) } }
        case .idle: return [(1, 1)]
        case .asleep: return [(0, 1), (1, 1), (2, 1)]
        }
    }

    /// A working mark breathes on its own clock, the frame read off the wall clock, so every
    /// working mark on screen shows the same frame and only these small canvases redraw.
    /// Under reduce motion it holds the still picture.
    var body: some View {
        Group {
            if kind == .working, !reduceMotion {
                TimelineView(.periodic(from: .now, by: 0.16)) { t in
                    canvas(DotMark.breath(Int(t.date.timeIntervalSinceReferenceDate / 0.16) % 32))
                }
            } else {
                canvas(dots)
            }
        }
        .frame(width: size, height: size)
        // Decorative: the row, section or count beside it already reads out the state.
        .accessibilityHidden(true)
    }

    private func canvas(_ dots: [(Int, Int)]) -> some View {
        Canvas { ctx, sz in
            let cell = sz.width / 3, r = cell * 0.32
            let ink = DotMark.ink(kind)
            for (x, y) in dots {
                let cx = cell * (CGFloat(x) + 0.5), cy = cell * (CGFloat(y) + 0.5)
                ctx.fill(Path(ellipseIn: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2)), with: .color(ink))
            }
        }
    }
}

/// The q mark, the same geometry as `docs/brand/make.py`'s `mark()`: the ring dead
/// centre, the tail hanging below it outside the centring, the only dot in ember.
/// Follows the settings window's own light or dark appearance, ground on cream or
/// cream on ground, the way the logo's two SVG variants do.
struct QMark: View {
    let size: CGFloat
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Canvas { ctx, _ in
            let step = size * 0.176, r = size * 0.0625
            let ox = size / 2 - step, oy = size / 2 - step
            let ink = scheme == .dark ? Palette.cream : Palette.ground
            let tailInk = scheme == .dark ? Palette.ember : Palette.emberOnLight
            for (x, y) in DotMark.ring {
                let cx = ox + CGFloat(x) * step, cy = oy + CGFloat(y) * step
                ctx.fill(Path(ellipseIn: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2)), with: .color(ink))
            }
            let tx = ox + 2 * step, ty = oy + 3 * step
            ctx.fill(Path(ellipseIn: CGRect(x: tx - r, y: ty - r, width: r * 2, height: r * 2)), with: .color(tailInk))
        }
        .frame(width: size, height: size)
        .accessibilityLabel("qoro")
    }
}

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
    let ink: Color
    var body: some View {
        Text("\(n)")
            .font(.system(size: 9, weight: .bold))
            .foregroundStyle(.black)
            .contentTransition(.numericText())
            .padding(.horizontal, 3.5)
            .frame(minWidth: 14, minHeight: 14)
            .background(Capsule().fill(ink))
            .overlay(Capsule().stroke(.black, lineWidth: 2))
            .scaleEffect(n > 0 ? 1 : 0.01)
            .opacity(n > 0 ? 1 : 0)
            .accessibilityLabel("\(n) need you")
    }
}

/// The right ear: a mark and its number per bucket shown, no words, so `[done] 1 ·
/// [working] 2 · [idle] 8` reads at a glance in the space words used to take.
struct Counts: View {
    @ObservedObject var s: Store
    /// The counts' own font size; the mark is sized off it, not a fixed constant.
    static let fontSize: CGFloat = 12
    /// As tall as the digits' cap height, roughly three quarters of the font size, so a
    /// mark never reads as a stray punctuation mark beside the number it belongs to.
    static let markSize: CGFloat = fontSize * 0.75
    /// The gap between a mark and its number.
    static let markGap: CGFloat = 3
    /// What separates one bucket from the next, now that there is no separator glyph to
    /// do it: about one digit's width at this font.
    static let bucketGap: CGFloat = 10

    var body: some View {
        HStack(spacing: Counts.bucketGap) {
            if s.finished > 0 {
                HStack(spacing: Counts.markGap) {
                    DotMark(kind: .done, size: Counts.markSize)
                    Text("\(s.finished)").contentTransition(.numericText())
                }
                .foregroundStyle(done)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(s.finished) done")
            }
            if s.working > 0 {
                HStack(spacing: Counts.markGap) {
                    DotMark(kind: .working, size: Counts.markSize)
                    Text("\(s.working)").contentTransition(.numericText())
                }
                .foregroundStyle(.white)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(s.working) working")
            }
            if s.idle > 0 {
                HStack(spacing: Counts.markGap) {
                    DotMark(kind: .idle, size: Counts.markSize)
                    Text("\(s.idle)").contentTransition(.numericText())
                }
                .foregroundStyle(.white.opacity(0.5))
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(s.idle) idle")
            }
        }
        .font(.system(size: Counts.fontSize, weight: .medium))
        .lineLimit(1)
        .fixedSize()
    }
}

struct RowView: View {
    let row: Row
    let open: Bool
    /// Name the row's session beside its number: set when more than one is shown.
    let session: Bool
    @State private var over = false

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(.white.opacity(0.45))
                .rotationEffect(.degrees(open ? 90 : 0))
            DotMark(kind: row.dotKind, size: 11)
            Text(row.label)
                .fontWeight(.medium)
                .foregroundStyle(row.ink ?? (row.kind == .idle ? .white.opacity(0.6) : .white))
            Text([row.place, row.detail].filter { !$0.isEmpty }.joined(separator: " \u{00b7} "))
                .foregroundStyle(.white.opacity(0.42))
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 8)
            Text(session ? "\(row.session)  \(row.terminal)" : "\(row.terminal)")
                .font(.system(size: 11, design: .monospaced))
                .lineLimit(1)
                .fixedSize()
                .foregroundStyle(.white.opacity(0.4))
        }
        .font(.system(size: 13))
        .padding(.horizontal, 8)
        .frame(height: 30)
        .background(RoundedRectangle(cornerRadius: 8).fill(
            row.ink?.opacity(over || open ? 0.18 : 0.1) ?? .white.opacity(over || open ? 0.08 : 0)))
        // The space's rail down the row's left edge, so rows of one space read as a block.
        .overlay(alignment: .leading) {
            RoundedRectangle(cornerRadius: 1).fill(Palette.space(row.workspace)).frame(width: 2).padding(.vertical, 3)
        }
        .contentShape(Rectangle())
        .onHover { over = $0 }
    }
}

/// What qoro would not read, in the interface's blocked ink. Prose, not terminal runs,
/// so it wraps at the width of the box's own text rather than scrolling off to the right
/// with the rest of the screen.
struct RefusalLine: View {
    let why: String

    var body: some View {
        Text(why)
            .font(.system(size: 11))
            .foregroundStyle(blocked)
            .fixedSize(horizontal: false, vertical: true)
            .frame(width: Geometry.peekText, alignment: .leading)
            .padding(.vertical, 2)
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
        if r.b == true { piece.font = .system(size: TermLine.size, weight: .bold, design: .monospaced) }
        if r.u == true { piece.underlineStyle = .single }
        if r.s == true { piece.strikethroughStyle = .single }
        out += piece
    }
    return Text(out)
}

/// A row as it reads in a box far narrower than its terminal, as the phone reads it. A
/// program lays its rows out for the Mac's width, so a row padded to the edge would wrap
/// into lines of blanks, and a box's right border would land alone on a line of its own.
/// The blank tail goes, unless something is painted on it, and so does a box border
/// standing after a gap: the inside reads the same without it. An ASCII `|` stays, since a
/// table needs it. Cut at the end only, so the rest of the row keeps its places.
func trimTail(_ runs: [Run]) -> [Run] {
    let text = runs.map(\.t).joined()
    var bare = Substring(text)
    while bare.last?.isWhitespace == true { bare.removeLast() }
    if let c = bare.last, "│┃║".contains(c), bare.dropLast().hasSuffix("  ") {
        bare.removeLast()
        while bare.last?.isWhitespace == true { bare.removeLast() }
    }
    // A background, inverse video, a line under or through all show on a blank, so a
    // coloured bar is kept to its last painted cell.
    var painted = 0, at = 0
    for r in runs {
        at += r.t.count
        if r.bg != nil || r.v == true || r.u == true || r.s == true { painted = at }
    }
    var left = max(bare.count, painted)
    guard left < at else { return runs }
    var out: [Run] = []
    for var r in runs where left > 0 {
        if r.t.count > left { r.t = String(r.t.prefix(left)) }
        left -= r.t.count
        out.append(r)
    }
    return out
}

/// A row drawn only of rule and box characters (the rules around an agent's input box, a
/// box's top and bottom): wrapped, a 224 column rule becomes four, so it is cut at the
/// edge. Box drawing only, and ASCII dashes and equals, never a pipe or an underscore a
/// row of text or a table could be made of.
func isRule(_ runs: [Run]) -> Bool {
    let text = runs.map(\.t).joined()
    return text.trimmingCharacters(in: .whitespaces).count >= 8
        && text.allSatisfy { $0.isWhitespace || "─━═╌┄-=│┃║╭╮╰╯┌┐└┘├┤┬┴┼╔╗╚╝".contains($0) }
}

/// A row as the peek draws it: at a size read at a glance whatever the terminal's width,
/// wrapped at the box's edge rather than scrolled sideways past it, since a peek is a
/// glance and a third of every row is not one. A rule is the exception, cut to one line.
struct TermLine: View {
    static let size: CGFloat = 10
    let runs: [Run]

    var body: some View {
        let runs = trimTail(self.runs)
        if isRule(runs) {
            // The zero minimum keeps the frame at the box's width: without it the frame
            // grows to the whole rule and the cut lands on the box's border instead.
            line(runs).lineLimit(1).fixedSize(horizontal: true, vertical: false)
                .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading).clipped()
        } else {
            line(runs).fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// What a peek sends to its terminal, in the order it was pressed. Letters typed faster
/// than one send takes go out together, so typing keeps up; keys never merge.
@MainActor
final class Keys {
    enum Op {
        case text(String)
        case key(String)
    }

    let tab: Tab
    unowned let store: Store
    private var queue: [Op] = []
    private var sending = false

    init(tab: Tab, store: Store) {
        self.tab = tab
        self.store = store
    }

    func push(_ op: Op) {
        if case .text(let more) = op, case .text(let have)? = queue.last {
            queue[queue.count - 1] = .text(have + more)
        } else {
            queue.append(op)
        }
        next()
    }

    private func next() {
        guard !sending, !queue.isEmpty else { return }
        sending = true
        let args: [String]
        switch queue.removeFirst() {
        case .text(let text): args = tab.qoro("send", String(tab.terminal), "--", text)
        case .key(let name): args = tab.qoro("send", String(tab.terminal), "--key", name)
        }
        let store = self.store
        DispatchQueue.global().async { [weak self] in
            let why = runQoroChecked(args, "qoro send")
            DispatchQueue.main.async {
                if let why { store.say(why) }
                self?.sending = false
                self?.next()
            }
        }
    }
}

/// A key press as the terminal should get it: a named key `qoro send --key` knows, or
/// the text it types. Nothing is read into it: arrows move through an agent's menu, Enter
/// picks, Esc cancels, the way they would in the terminal itself.
func keyOp(_ press: KeyPress) -> Keys.Op? {
    if press.modifiers.contains(.command) {
        // The one command key that types: paste. Line ends made one kind, no control
        // characters, and several lines marked as a paste the way a terminal marks one, so
        // a newline inside it is text rather than the Enter that sends a prompt.
        guard press.key.character == "v", let raw = NSPasteboard.general.string(forType: .string) else { return nil }
        let text = raw.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
            .filter { $0 == "\n" || $0 == "\t" || !($0.asciiValue.map { $0 < 0x20 || $0 == 0x7f } ?? false) }
        guard !text.isEmpty else { return nil }
        return .text(text.contains("\n") ? "\u{1b}[200~" + text + "\u{1b}[201~" : text)
    }
    let named: [KeyEquivalent: String] = [
        .return: "enter", .escape: "esc", .upArrow: "up", .downArrow: "down",
        .leftArrow: "left", .rightArrow: "right", .home: "home", .end: "end", .pageUp: "pageup",
        .pageDown: "pagedown",
    ]
    // Shift-Tab reaches here as backtab, U+0019, as often as it does as a shifted Tab.
    if press.key == .tab || press.key.character == "\u{19}" {
        return .key(press.modifiers.contains(.shift) || press.key.character == "\u{19}" ? "shift-tab" : "tab")
    }
    // A new line rather than a send, the way agents take Esc then Enter.
    if press.key == .return, !press.modifiers.isDisjoint(with: [.shift, .option]) { return .text("\u{1b}\r") }
    // Backspace is DEL (U+007F) from a Mac keyboard; SwiftUI's `.delete` is U+0008. Either
    // one rubs out a character, and with Option a word, as a terminal's meta key does.
    if press.key == .delete || press.key.character == "\u{7f}" {
        return press.modifiers.contains(.option) ? .text("\u{1b}\u{7f}") : .key("backspace")
    }
    if let name = named[press.key] { return .key(name) }
    if press.modifiers.contains(.control) {
        let c = press.key.character.lowercased()
        guard let ch = c.first, ch.isASCII, ch.isLetter else { return nil }
        return .key("ctrl-\(ch)")
    }
    // No control characters, and none of the private codes AppKit gives function keys and
    // forward delete, which would arrive as three bytes of nothing.
    let text = press.characters.filter { c in
        !(c.asciiValue.map { $0 < 0x20 || $0 == 0x7f } ?? false)
            && !c.unicodeScalars.contains { (0xF700...0xF8FF).contains($0.value) }
    }
    return text.isEmpty ? nil : .text(text)
}

/// The terminal of an open row: live, scrollable back into what came before, and, once
/// clicked, typed into directly, every key going to that terminal as pressed.
struct TermPeek: View {
    static let height: CGFloat = 190
    @ObservedObject var m: Mirror
    let row: Row
    @ObservedObject var store: Store
    let open: (Tab) -> Void
    @FocusState private var typing: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ScrollView(.vertical, showsIndicators: false) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if m.more {
                        Text("earlier")
                            .foregroundStyle(.white.opacity(0.3))
                            .onAppear { m.loadOlder() }
                    }
                    // What qoro would not read stands where the past is, and a read that
                    // answers takes its place: a sentence and the rows it is about are never
                    // both on the box. A watch that died leaves the last screen standing for
                    // a terminal nobody is reading any more, so it gives the screen up.
                    if m.screenRefusal == nil, let why = m.pageRefusal {
                        RefusalLine(why: why)
                    }
                    ForEach(m.older) { TermLine(runs: $0.runs) }
                    if !m.older.isEmpty {
                        Rectangle().fill(.white.opacity(0.12)).frame(height: 1).padding(.vertical, 4)
                    }
                    if let why = m.screenRefusal {
                        RefusalLine(why: why)
                    } else {
                        ForEach(m.screen) { TermLine(runs: $0.runs) }
                    }
                }
                .font(.system(size: TermLine.size, design: .monospaced))
                .padding(8)
            }
            .defaultScrollAnchor(.bottomLeading)
            .frame(height: TermPeek.height)
            .background(RoundedRectangle(cornerRadius: 10).fill(Color(white: 0.05)))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(typing ? focus.opacity(0.8) : .white.opacity(0.08), lineWidth: typing ? 1.5 : 1))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .focusable()
            .focusEffectDisabled()
            .focused($typing)
            .onTapGesture {
                store.takeKeyboard?()
                typing = true
            }
            .onKeyPress(phases: [.down, .repeat]) { press in
                // A key the terminal has no use for is still taken while typing here, rather
                // than passed up to the system, which answers it with the error beep. Command
                // keys go on, so the system's own shortcuts keep working.
                guard let op = keyOp(press) else {
                    return press.modifiers.contains(.command) ? .ignored : .handled
                }
                store.keys(for: row.tab).push(op)
                return .handled
            }
            HStack(spacing: 8) {
                if let note = store.note {
                    Text(note).foregroundStyle(blocked).lineLimit(1).truncationMode(.tail)
                } else {
                    Text(typing ? "typing into terminal \(row.terminal); click outside to stop" : "click to type into terminal \(row.terminal)")
                        .foregroundStyle(typing ? focus.opacity(0.9) : .white.opacity(0.4))
                }
                Spacer(minLength: 8)
                Button { open(row.tab) } label: {
                    Label("open in qoro", systemImage: "arrow.up.right")
                        .labelStyle(.titleAndIcon)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.white.opacity(0.55))
                .fixedSize()
            }
            .font(.system(size: 11.5))
        }
        .onChange(of: typing) { _, now in
            if now {
                store.typingTo = row.tab
            } else if store.typingTo == row.tab {
                store.typingTo = nil
                store.release?()
            }
        }
        // A row moving between sections (answered, it goes from "needs you" to "working")
        // builds its peek again: typing carries on in the new one.
        .onAppear { if store.typingTo == row.tab { typing = true } }
        .onDisappear {
            if store.expanded != row.tab, store.typingTo == row.tab {
                store.typingTo = nil
                store.release?()
            }
        }
    }
}

/// The person's first name, lower case, as the island's header greets them.
let personName = NSFullUserName().split(separator: " ").first.map(String.init)?.lowercased() ?? NSUserName()

struct Island: View {
    @ObservedObject var s: Store
    let open: (Tab) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Divider().overlay(.white.opacity(0.08)).padding(.top, 4).padding(.bottom, 2)
            if let note = s.note, s.expanded == nil {
                Text(note)
                    .font(.system(size: 12))
                    .foregroundStyle(blocked)
                    .lineLimit(2)
                    .padding(.top, 6)
                    .transition(.opacity)
            }
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 0) {
                    section("needs you", .waiting, s.needKind)
                    section("done", .done, .done)
                    section("working", .working, .working)
                    section("idle", .idle, .idle)
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
    private func section(_ title: String, _ kind: Row.Kind, _ dot: DotMark.Kind) -> some View {
        let rows = s.rows.filter { $0.kind == kind }
        if !rows.isEmpty {
            HStack(spacing: 5) {
                DotMark(kind: dot, size: 10)
                Text(title.uppercased())
                    .font(.system(size: 10.5, design: .monospaced))
                    .tracking(0.6)
                    .foregroundStyle(.white.opacity(0.38))
            }
            .padding(.top, 8).padding(.bottom, 3)
            ForEach(rows) { row in
                let isOpen = s.expanded == row.tab
                RowView(row: row, open: isOpen, session: s.mixed)
                    .padding(.horizontal, -8)
                    .onTapGesture { withAnimation(spring) { s.expanded = isOpen ? nil : row.tab } }
                    .accessibilityElement(children: .combine)
                    .accessibilityAddTraits(.isButton)
                    .accessibilityHint(isOpen ? "Hides terminal \(row.terminal)" : "Shows terminal \(row.terminal) here")
                    .accessibilityAction { s.expanded = isOpen ? nil : row.tab }
                if isOpen, let mirror = s.mirror {
                    TermPeek(m: mirror, row: row, store: s, open: open)
                        .padding(.top, 4).padding(.bottom, 8)
                        .transition(.opacity.combined(with: .offset(y: -8)))
                }
            }
        }
    }
}

// MARK: settings

/// `qoro config --json`: every setting in effect, keyed as `qoro config set` takes them.
struct ConfigRead: Decodable {
    struct Values: Decodable {
        var prefix: String
        var theme: String
        var desktop: Bool
        var sound: Bool
        var notch: Bool

        enum CodingKeys: String, CodingKey {
            case prefix, theme
            case desktop = "notify.desktop", sound = "notify.sound", notch = "notch.enabled"
        }
    }
    struct Choices: Decodable {
        let prefix: [String]
        let theme: [String]
    }
    let path: String
    let exists: Bool
    var settings: Values
    let choices: Choices
    let problems: [String]
}

/// `qoro avatar options --json`: every part a pet may have.
struct PetOptions: Decodable {
    struct Named: Decodable {
        let id: String
        let title: String
    }
    struct Ink: Decodable {
        let ink: Int
        let title: String
    }
    let hair: [Named]
    let hair_ink: [Ink]
    let eyes: [Named]
}

/// The parts of the pet now, from `qoro avatar show --json`.
struct PetParts: Decodable {
    var hair: String
    var hair_ink: Int
    var eyes: String
}

func readConfig() -> ConfigRead? {
    runQoro(["config", "--json"]).flatMap { try? JSONDecoder().decode(ConfigRead.self, from: $0) }
}

/// What the settings window shows, read from qoro and written back through it one
/// setting at a time, in the order they were changed.
@MainActor
final class Settings: ObservableObject {
    @Published var config: ConfigRead?
    @Published var options: PetOptions?
    @Published var parts: PetParts?
    @Published var error: String?
    unowned let store: Store
    /// Told when the notch is turned on or off here, so it shows or hides at once.
    var notch: ((Bool) -> Void)?
    /// One write at a time: two quick changes must not read the file before the other wrote.
    private let writes = DispatchQueue(label: "qoro.settings")

    init(store: Store) { self.store = store }

    /// Read on the write queue, so a read never lands before a write made ahead of it.
    func load() {
        writes.async { [weak self] in
            let config = readConfig()
            let options = runQoro(["avatar", "options", "--json"]).flatMap { try? JSONDecoder().decode(PetOptions.self, from: $0) }
            let parts = runQoro(["avatar", "show", "--json"]).flatMap { try? JSONDecoder().decode(PetParts.self, from: $0) }
            DispatchQueue.main.async {
                self?.config = config
                self?.options = options
                self?.parts = parts
            }
        }
    }

    func set(_ key: String, _ value: String) {
        write(["config", "set", key, value]) { [weak self] written in
            // Only once it is on disk: a write refused leaves the notch as it was.
            if written, key == "notch.enabled" { self?.notch?(value == "true") }
        }
    }

    func pet(_ flag: String, _ value: String) {
        write(["avatar", "set", flag, value]) { [weak self] _ in
            if let store = self?.store { loadPet(into: store) }
        }
    }

    /// Wait for every write already asked for.
    func settle() { writes.sync {} }

    private func write(_ args: [String], then: ((Bool) -> Void)? = nil) {
        writes.async { [weak self] in
            let why = runQoroChecked(args, "qoro config set")
            DispatchQueue.main.async {
                self?.error = why
                then?(why == nil)
                self?.load()
            }
        }
    }
}

/// The settings window: the system's own grouped form, nothing drawn by hand.
struct SettingsView: View {
    @ObservedObject var m: Settings
    @ObservedObject var store: Store

    var body: some View {
        VStack(spacing: 0) {
            QMark(size: 48).padding(.top, 22).padding(.bottom, 6)
            Form {
                if let c = m.config {
                    content(c)
                } else {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .formStyle(.grouped)
        }
        .frame(width: 460, height: 640)
    }

    @ViewBuilder
    private func content(_ c: ConfigRead) -> some View {
        Section("Notch") {
            Toggle("Show agents in the notch", isOn: flag("notch.enabled", \.notch))
        }
        Section {
            Toggle("Desktop notification", isOn: flag("notify.desktop", \.desktop))
            Toggle("Sound", isOn: flag("notify.sound", \.sound))
        } header: {
            Text("When an agent needs you")
        }
        Section("Appearance") {
            Picker("Theme", selection: choice("theme", \.theme)) {
                ForEach(c.choices.theme, id: \.self) { Text($0.capitalized).tag($0) }
            }
            .pickerStyle(.segmented)
        }
        Section {
            Picker("Prefix key", selection: choice("prefix", \.prefix)) {
                ForEach(c.choices.prefix, id: \.self) { Text($0).tag($0) }
            }
        } header: {
            Text("Keyboard")
        } footer: {
            Text("The key that starts every qoro command.")
        }
        if let o = m.options {
            Section("Pet") {
                HStack {
                    Spacer()
                    PetView(pixels: store.pixels, px: 5)
                    Spacer()
                }
                .padding(.vertical, 6)
                Picker("Hair", selection: part(\.hair, "--hair", "block")) {
                    ForEach(o.hair, id: \.id) { Text($0.title).tag($0.id) }
                }
                Picker("Colour", selection: inkPart()) {
                    ForEach(o.hair_ink, id: \.ink) { Text($0.title).tag($0.ink) }
                }
                Picker("Eyes", selection: part(\.eyes, "--eyes", "big")) {
                    ForEach(o.eyes, id: \.id) { Text($0.title).tag($0.id) }
                }
            }
        }
        if !c.problems.isEmpty || m.error != nil {
            Section("Could not be used") {
                ForEach(c.problems + [m.error].compactMap { $0 }, id: \.self) {
                    Text($0).foregroundStyle(.red)
                }
            }
        }
        Section {
            LabeledContent("File") {
                Text(c.exists ? c.path : "no file yet: every setting is its default")
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
            }
            HStack {
                Spacer()
                Button("Show in Finder") {
                    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: c.path)])
                }
                .disabled(!c.exists)
                Button("Open config.toml") {
                    NSWorkspace.shared.open(URL(fileURLWithPath: c.path))
                }
                .disabled(!c.exists)
            }
        }
    }

    /// A switch shown as changed at once, then written.
    private func flag(_ key: String, _ path: WritableKeyPath<ConfigRead.Values, Bool>) -> Binding<Bool> {
        Binding(
            get: { m.config?.settings[keyPath: path] ?? false },
            set: {
                m.config?.settings[keyPath: path] = $0
                m.set(key, $0 ? "true" : "false")
            })
    }

    private func choice(_ key: String, _ path: WritableKeyPath<ConfigRead.Values, String>) -> Binding<String> {
        Binding(
            get: { m.config?.settings[keyPath: path] ?? "" },
            set: {
                m.config?.settings[keyPath: path] = $0
                m.set(key, $0)
            })
    }

    /// A part of the pet; with none picked yet, the picker starts where `qoro avatar set` does.
    private func part(_ path: WritableKeyPath<PetParts, String>, _ flag: String, _ none: String) -> Binding<String> {
        Binding(
            get: { m.parts?[keyPath: path] ?? none },
            set: {
                m.parts?[keyPath: path] = $0
                m.pet(flag, $0)
            })
    }

    private func inkPart() -> Binding<Int> {
        Binding(
            get: { m.parts?.hair_ink ?? 212 },
            set: {
                m.parts?.hair_ink = $0
                m.pet("--hair-ink", String($0))
            })
    }
}

/// Sizes the panel reads to know where clicks belong to the notch.
@MainActor
struct Geometry {
    var notch: CGSize
    static let openWidth: CGFloat = 500
    static let rowHeight: CGFloat = 30
    /// What a sentence has to wrap inside the peek's box: the open island, less the
    /// island's own padding and the box's.
    static let peekText: CGFloat = openWidth - 40 - 16
    /// The tallest the inbox grows; past it the list scrolls. Inside the panel's 700.
    static let openMaxHeight: CGFloat = 640
    /// The left ear holds the pet and the badge beside it, whatever the right one says.
    static let petEar: CGFloat = 62
    /// The gear beside the counts once open, with its gap.
    static let gear: CGFloat = 28

    /// The right ear's width: not the sentence `countsText` was measured as, but what
    /// `Counts` now draws, a mark and a number per bucket shown, spaced apart with no
    /// separator glyph. Each bucket's own width is the fixed mark plus its gap plus the
    /// measured number, so the pill never clips a mark or crowds a number.
    func closedWidth(_ s: Store) -> CGFloat {
        guard !s.rows.isEmpty else { return notch.width }
        let counts = [s.finished, s.working, s.idle].filter { $0 > 0 }
        guard !counts.isEmpty else { return notch.width + 2 * Geometry.petEar }
        let font: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: Counts.fontSize, weight: .medium)]
        func width(_ text: String) -> CGFloat { ceil(NSAttributedString(string: text, attributes: font).size().width) }
        let buckets = counts.reduce(CGFloat(0)) { $0 + Counts.markSize + Counts.markGap + width("\($1)") }
        let gaps = CGFloat(counts.count - 1) * Counts.bucketGap
        let ear = max(Geometry.petEar, ceil(buckets + gaps) + 26)
        return notch.width + 2 * ear
    }

    func openHeight(_ s: Store) -> CGFloat {
        let sections = [Row.Kind.waiting, .done, .working, .idle].filter { k in s.rows.contains { $0.kind == k } }.count
        let peek: CGFloat = s.expanded == nil ? 0 : TermPeek.height + 6 + 18 + 12
        let note: CGFloat = s.note == nil || s.expanded != nil ? 0 : 26
        let full = notch.height + 20 + CGFloat(sections) * 25 + CGFloat(s.rows.count) * Geometry.rowHeight + 4 + peek + note
        return min(full, Geometry.openMaxHeight)
    }

    func size(_ s: Store) -> CGSize {
        s.open
            ? CGSize(width: max(Geometry.openWidth, closedWidth(s) + 2 * Geometry.gear), height: openHeight(s))
            : CGSize(width: closedWidth(s) + (s.hover && !s.rows.isEmpty ? 8 : 0), height: notch.height + (s.hover && !s.rows.isEmpty ? 3 : 0))
    }
}

struct NotchView: View {
    @ObservedObject var s: Store
    let g: Geometry
    let open: (Tab) -> Void
    let settings: () -> Void

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
                        Badge(n: s.waiting, ink: s.needInk).offset(x: 14, y: -5)
                    }
                    .padding(.leading, 20)
                    // Open, the ears grow into the island's header: who, then the gear.
                    if s.open {
                        Text(personName)
                            .font(.system(size: 13, weight: .semibold)).foregroundStyle(.white)
                            .lineLimit(1)
                            .padding(.leading, 12)
                            .transition(.opacity)
                    }
                    Spacer(minLength: g.notch.width)
                    Counts(s: s)
                    if s.open {
                        Button(action: settings) {
                            Image(systemName: "gearshape")
                                .font(.system(size: 12))
                                .foregroundStyle(.white.opacity(0.5))
                                .frame(width: 22, height: 22)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .help("Settings")
                        .accessibilityLabel("Settings")
                        .padding(.leading, 6)
                        .transition(.opacity)
                    }
                }
                .padding(.trailing, s.open ? 14 : 20)
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
    // Key only so a peek can take typing; being non-activating, it never takes
    // the app to the front or the focus from the terminal under it until clicked in.
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class App: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let store = Store()
    lazy var settings = Settings(store: store)
    var settingsWindow: NSWindow?
    var panel: NotchPanel!
    var geometry = Geometry(notch: CGSize(width: 185, height: 32))
    var monitors: [Any] = []
    var closing = false
    var folded: AnyCancellable?

    func applicationDidFinishLaunching(_ note: Notification) {
        // Turned off in its settings: nothing to draw, and an exit that launchd leaves down
        // until `qoro notch install` or the setting turns it back on.
        if readConfig()?.settings.notch == false { exit(0) }
        settings.notch = { [weak self] on in self?.show(on) }
        panel = NotchPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.level = .statusBar + 1
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        panel.ignoresMouseEvents = true
        // Key only when a peek is clicked to be typed into; a click on a row or the notch
        // leaves the keyboard where it was.
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
        // Folding hands the keyboard back: a panel left key after the peek is gone
        // would swallow what the person types next into the app under it.
        store.release = { [weak self] in self?.release() }
        store.takeKeyboard = { [weak self] in self?.panel.makeKey() }
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
        let host = NSHostingView(rootView: NotchView(
            s: store, g: geometry,
            open: { [weak self] t in self?.go(t) },
            settings: { [weak self] in self?.openSettings() }))
        // The panel keeps the size given here. By default a hosting view resizes its window
        // to fit what it draws, which moved the notch off the camera as the ears grew.
        host.sizingOptions = []
        panel.contentView = host
        // As wide as the menu bar: the island grows with its counts, and the panel takes the
        // mouse only over the black, so the rest of the width costs nothing.
        let w = f.width, h: CGFloat = 700
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
        // Not while typing: the peek holds the keyboard, and a pointer drifting off the
        // edge must not take the half written answer with it.
        if store.open, !over, !closing, !panel.isKeyWindow {
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

    func go(_ tab: Tab) {
        jump(to: tab, store: store)
    }

    /// The settings window: a plain titled window in front, opened from the island's gear.
    func openSettings() {
        store.open = false
        if settingsWindow == nil {
            let window = NSWindow(contentViewController: NSHostingController(rootView: SettingsView(m: settings, store: store)))
            window.title = "qoro Settings"
            window.styleMask = [.titled, .closable]
            window.isReleasedWhenClosed = false
            window.delegate = self
            settingsWindow = window
            // No menu bar in an app like this one, so cmd-W is heard here.
            monitors.append(NSEvent.addLocalMonitorForEvents(matching: .keyDown) { e in
                guard e.modifierFlags.contains(.command), e.charactersIgnoringModifiers == "w",
                      e.window === window else { return e }
                window.performClose(nil)
                return nil
            } as Any)
        }
        settings.load()
        settingsWindow?.center()
        NSApp.activate()
        settingsWindow?.makeKeyAndOrderFront(nil)
    }

    /// Shows or hides the notch at once, when it is turned on or off in the settings.
    func show(_ on: Bool) {
        if on {
            panel.orderFrontRegardless()
        } else {
            store.open = false
            panel.orderOut(nil)
        }
    }

    /// Closing the settings with the notch off leaves nothing to show: the app ends. Decided
    /// by the file once every write has landed, not by what the window last showed.
    func windowWillClose(_ notification: Notification) {
        settings.settle()
        if readConfig()?.settings.notch == false { NSApp.terminate(nil) }
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
