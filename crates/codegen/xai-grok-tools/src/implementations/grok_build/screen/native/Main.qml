import QtQuick
import QtQuick.Window
import QtWebEngine

Window {
    id: window
    visible: false
    width: 800
    height: 600
    color: "transparent"
    flags: Qt.FramelessWindowHint
    title: "Grok desktop overlay"
    property bool canReset: false

    WebEngineProfilePrototype {
        id: privateProfile
        httpCacheType: WebEngineProfile.MemoryHttpCache
        persistentCookiesPolicy: WebEngineProfile.NoPersistentCookies
        Component.onCompleted: web.active = true
    }
    // The prototype's instance is null until component completion. Defer view
    // creation so Qt never receives a null profile during property initialization.
    Loader {
        id: web
        anchors.fill: parent
        active: false
        sourceComponent: Component {
            WebEngineView {
                url: "qrc:/overlay.html"
                backgroundColor: "transparent"
                profile: privateProfile.instance()
                settings.localContentCanAccessRemoteUrls: false
                settings.localContentCanAccessFileUrls: false
                settings.javascriptCanOpenWindows: false
                onContextMenuRequested: request => request.accepted = true
                onPermissionRequested: permission => permission.deny()
                onNavigationRequested: request => {
                    if (request.url.toString() !== "qrc:/overlay.html") request.action = WebEngineNavigationRequest.IgnoreRequest
                }
                onLoadingChanged: request => {
                    if (request.status === WebEngineView.LoadSucceededStatus) backend.ready = true
                    else if (request.status === WebEngineView.LoadFailedStatus) backend.fail("Cannot load the bundled renderer")
                }
            }
        }
    }

    component OverlayButton: Rectangle {
        required property string label
        signal clicked()
        width: 80
        height: 30
        color: "#202936"
        border.color: "#8291ab"
        Text { anchors.centerIn: parent; text: parent.label; color: "white"; font.pixelSize: 14 }
        MouseArea { anchors.fill: parent; onClicked: parent.clicked() }
    }
    OverlayButton {
        id: close
        x: window.width - width - 12
        y: 12
        label: qsTr("Close ×")
        onClicked: backend.dismiss()
    }
    OverlayButton {
        id: reset
        x: close.x - width - 8
        y: close.y
        visible: window.canReset
        label: qsTr("Reset")
        onClicked: web.item?.runJavaScript("globalThis.__grokOverlay?.resetView?.()")
    }
    Shortcut { sequence: "Escape"; context: Qt.WindowShortcut; onActivated: backend.dismiss() }

    Connections {
        target: backend
        function onDraw(scene) {
            window.canReset = false
            web.item.runJavaScript("(() => { try { globalThis.__grokDraw(" + scene + "); return {ok:true}; } catch(e) { return {error:String(e)}; } })()", result => {
                if (!result || result.error) backend.fail(result?.error ?? "Renderer failed")
                else backend.finish()
            })
        }
        function onClear() {
            window.canReset = false
            web.item?.runJavaScript("globalThis.__grokOverlay?.clear()")
        }
    }
    Timer {
        interval: 50
        repeat: true
        running: window.visible && backend.ready
        onTriggered: web.item.runJavaScript("({active:!!globalThis.__grokOverlay?.host, bounds:globalThis.__grokOverlay?.interactiveBounds ?? [], canReset:typeof globalThis.__grokOverlay?.resetView === 'function'})", result => {
            if (!result) return
            window.canReset = result.canReset
            if (!result.active && !backend.pending) backend.dismiss()
            else {
                const areas = result.bounds
                if (window.canReset) areas.push([reset.x, reset.y, reset.width, reset.height])
                backend.updateInput(areas)
            }
        })
    }
}
