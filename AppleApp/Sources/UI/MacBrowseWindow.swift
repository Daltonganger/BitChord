#if os(macOS)
import SwiftUI

/// Values carried by the shared browse window scene. Search and collection
/// windows are independent windows, but use the same player and account models.
enum MacBrowseWindowRoute: Codable, Hashable {
    case detail(browseId: String, title: String)
    case search(query: String, scope: SearchView.Scope, source: SearchView.SearchSource)
    case grid(title: String, items: [ShelfCard])
}

enum MacBrowseWindow {
    static let sceneID = "mac-browse-window"
}

struct MacBrowseWindowView: View {
    let route: MacBrowseWindowRoute?

    var body: some View {
        NavigationStack {
            destination
                .navigationDestination(for: PushRoute.self) { $0.make() }
        }
        .environment(\.inBrowseStack, true)
        .frame(minWidth: 700, minHeight: 560)
    }

    @ViewBuilder
    private var destination: some View {
        switch route {
        case .detail(let browseId, let title):
            DetailView(browseId: browseId, initialTitle: title)
        case .search(let query, let scope, let source):
            SearchView(initialQuery: query, initialScope: scope, initialSource: source)
        case .grid(let title, let items):
            LibraryGridView(title: title, items: items)
        case nil:
            ContentUnavailableView("Nothing to Show", systemImage: "macwindow")
        }
    }
}
#endif
