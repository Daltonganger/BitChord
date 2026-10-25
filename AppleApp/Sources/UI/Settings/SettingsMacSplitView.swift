import SwiftUI

/// A two-pane shell for the Mac Settings screen. The owning view supplies its
/// existing section model and filtered detail content, so navigation stays in
/// sync with Settings search and deep links.
struct SettingsMacSplitView<Section: Hashable & Identifiable, SidebarLabel: View, Detail: View>: View {
    let sections: [Section]
    @Binding var selection: Section?
    @ViewBuilder let sidebarLabel: (Section) -> SidebarLabel
    @ViewBuilder let detail: (Section) -> Detail

    var body: some View {
        NavigationSplitView {
            List(selection: $selection) {
                ForEach(sections) { section in
                    sidebarLabel(section)
                }
            }
            .listStyle(.sidebar)
            .navigationSplitViewColumnWidth(min: 190, ideal: 240)
        } detail: {
            if let selection, sections.contains(where: { $0.id == selection.id }) {
                detail(selection)
            } else {
                ContentUnavailableView("No Settings", systemImage: "gearshape")
            }
        }
    }
}
