// Native window host only. Rust validates tool arguments and owns this process.
#include <QGuiApplication>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickWindow>
#include <QScreen>
#include <QSocketNotifier>
#include <QTimer>
#include <QtWebEngineQuick>
#ifdef OVERLAY_SELF_TEST
#include <QWheelEvent>
#include <QtTest/QTest>
#endif
#include <LayerShellQt/Window>
#include <cstdio>
#include <fcntl.h>
#include <memory>
#include <unistd.h>

class Backend final : public QObject {
  Q_OBJECT
  Q_PROPERTY(bool ready READ ready WRITE setReady NOTIFY readyChanged)
  Q_PROPERTY(bool pending READ pending NOTIFY pendingChanged)
public:
  QQuickWindow *window = nullptr;
  bool ready() const { return m_ready; }
  bool pending() const { return m_pending; }
  void setReady(bool value) {
    if (m_ready != value) {
      m_ready = value;
      emit readyChanged();
    }
  }

  void request(const QByteArray &line) {
    if (m_pending) {
      reject("A drawing is already pending");
      return;
    }
    QJsonParseError error;
    const auto document = QJsonDocument::fromJson(line, &error);
    if (error.error != QJsonParseError::NoError || !document.isObject()) {
      reject("Invalid overlay JSON");
      return;
    }
    const auto input = document.object();
    if (input.value("action") == "clear") {
      const auto output = input.value("output").toString();
      if (!output.isEmpty() && window->isVisible() &&
          output != window->screen()->name()) {
        reject("This session has no overlay on that output");
        return;
      }
      dismiss();
      reply({{"output", window->screen()->name()}, {"state", "cleared"}});
      return;
    }
    if (input.value("action") != "show" || !input.value("scene").isObject()) {
      reject("Expected show with a scene, or clear");
      return;
    }
    const auto output = input.value("output").toString();
    QScreen *screen = nullptr;
    if (!output.isEmpty()) {
      for (auto candidate : QGuiApplication::screens()) {
        if (candidate->name() == output) {
          screen = candidate;
          break;
        }
      }
      if (!screen) {
        reject("Unknown desktop output: " + output);
        return;
      }
    }
    m_pending = true;
    emit pendingChanged();
    m_scene = QString::fromUtf8(QJsonDocument(input.value("scene").toObject())
                                    .toJson(QJsonDocument::Compact));
    emit clear();
    auto layer = LayerShellQt::Window::get(window);
    // A layer surface's output binding is fixed when it is created. Keep an
    // exposed surface on the same output: destroying it under WebEngine can
    // invalidate the GPU context of the next drawing.
    const bool sameOutput = !screen || screen == window->screen();
    if (!window->isVisible() || !sameOutput) {
      window->hide();
      window->releaseResources();
      window->destroy();
      if (screen) {
        window->setScreen(screen);
        layer->setScreen(screen);
      } else {
        layer->setWantsToBeOnActiveScreen(true);
      }
    }
    layer->setActivateOnShow(false);
    layer->setKeyboardInteractivity(
        LayerShellQt::Window::KeyboardInteractivityOnDemand);
    window->resize((screen ? screen : window->screen())->size());
    updateInput({});
    window->show();
    dispatch();
  }

  Q_INVOKABLE void finish() {
    if (!m_pending)
      return;
    // Allow the native compositor to present the render before acknowledging
    // it.
    QTimer::singleShot(100, this, [this] {
      if (!m_pending)
        return;
      m_pending = false;
      emit pendingChanged();
      reply({{"output", window->screen()->name()}, {"state", "visible"}});
    });
  }
  Q_INVOKABLE void fail(const QString &message) {
    if (m_pending) {
      m_pending = false;
      emit pendingChanged();
    }
    if (window) {
      emit clear();
      window->hide();
    }
    reply({{"error", message}, {"kind", "fatal"}});
  }
  Q_INVOKABLE void dismiss() {
    emit clear();
    window->hide();
    if (m_pending) {
      m_pending = false;
      emit pendingChanged();
      reply({{"error", "Drawing was dismissed before completion"},
             {"kind", "fatal"}});
    }
  }
  Q_INVOKABLE void updateInput(const QVariantList &areas) {
    if (!window || QGuiApplication::mouseButtons() != Qt::NoButton)
      return;
    // On Wayland Qt's window mask is the surface input region. Always keep a
    // close button reachable; all other pixels outside interactive areas pass
    // through.
    QRegion region(window->width() - 92, 12, 80, 30);
    for (const auto &value : areas) {
      const auto area = value.toList();
      if (area.size() != 4)
        continue;
      region += QRectF(area[0].toDouble(), area[1].toDouble(),
                       area[2].toDouble(), area[3].toDouble())
                    .toAlignedRect();
    }
    window->setMask(region);
  }
signals:
  void draw(const QString &scene);
  void clear();
  void pendingChanged();
  void readyChanged();

private:
  bool m_ready = false;
  bool m_pending = false;
  QString m_scene;
  static void reject(const QString &message) {
    reply({{"error", message}, {"kind", "rejected"}});
  }
  void dispatch() {
    if (!m_pending)
      return;
    if (!m_ready || !window->isExposed()) {
      QTimer::singleShot(25, this, [this] { dispatch(); });
      return;
    }
    emit draw(m_scene);
  }
  static void reply(const QJsonObject &object) {
    const auto data =
        QJsonDocument(object).toJson(QJsonDocument::Compact) + '\n';
    std::fwrite(data.constData(), 1, static_cast<size_t>(data.size()), stdout);
    std::fflush(stdout);
  }
};

#ifdef OVERLAY_SELF_TEST
static void testOverlay(Backend &backend, QGuiApplication &app) {
  auto timer = new QTimer(&app);
  timer->setInterval(400);
  auto stage = std::make_shared<int>(0);
  auto image = std::make_shared<QImage>();
  auto region = std::make_shared<QRegion>();
  auto initialRegion = std::make_shared<QRegion>();
  const auto check = [&app](bool valid, const char *message) {
    if (!valid) {
      qCritical("Native overlay test failed: %s", message);
      app.exit(1);
    }
    return valid;
  };
  backend.request(
      R"({"action":"show","scene":{"kind":"annotations","items":[{"kind":"rectangle","x":40,"y":80,"width":200,"height":100,"color":"#ff3366"},{"kind":"text","x":50,"y":60,"text":"Native overlay fixture","color":"#ffffff"}]}})");
  QObject::connect(
      timer, &QTimer::timeout, &app,
      [&, stage, image, region, initialRegion, check] {
        if (backend.pending() || !backend.ready())
          return;
        auto window = backend.window;
        if (*stage == 0) {
          const auto shot = window->grabWindow();
          const double scale = double(shot.width()) / window->width();
          if (!check(!shot.isNull(), "Native capture is empty"))
            return;
          const auto stroke =
              shot.pixelColor(qRound(40 * scale), qRound(110 * scale));
          if (!check(stroke.red() > 220 && stroke.green() < 100,
                     "Annotation is missing from native pixels"))
            return;
          if (!check(shot.pixelColor(qRound(10 * scale), qRound(300 * scale))
                             .alpha() == 0,
                     "Background must be transparent"))
            return;
          if (!check(!window->mask().contains(QPoint(300, 300)),
                     "Annotations must pass background clicks through"))
            return;
          if (!check(window->mask().contains(QPoint(window->width() - 50, 25)),
                     "Close button must receive input"))
            return;
          backend.request(
              R"({"action":"show","scene":{"kind":"model3d","parts":[{"primitive":"sphere","position":[0,0,0],"rotation":[0,0,0],"scale":[2,2,2],"color":"#ff3366"},{"primitive":"torus","position":[0,0,0],"rotation":[1.3,0,0],"scale":[3,3,3],"color":"#88aaff"},{"primitive":"box","position":[-2,0,0],"rotation":[0,0,0],"scale":[1,1,1],"color":"#ffffff"},{"primitive":"cylinder","position":[2,0,0],"rotation":[0,0,0],"scale":[1,1,1],"color":"#ffaa88"},{"primitive":"cone","position":[0,2,0],"rotation":[0,0,0],"scale":[1,1,1],"color":"#88ffaa"}]}})");
          *stage = 1;
        } else if (*stage == 1) {
          if (!check(window->mask().contains(
                         QPoint(window->width() / 2, window->height() / 2)),
                     "3D objects must receive input"))
            return;
          *image = window->grabWindow();
          *initialRegion = window->mask();
          if (!check(window->mask().contains(QPoint(window->width() - 138, 25)),
                     "3D Reset button must receive input"))
            return;
          bool centered = false;
          for (int y = image->height() / 4;
               y < image->height() * 3 / 4 && !centered; y += 4) {
            for (int x = image->width() / 4; x < image->width() * 3 / 4;
                 x += 4) {
              if (image->pixelColor(x, y).alpha() > 0) {
                centered = true;
                break;
              }
            }
          }
          if (!check(centered, "3D geometry must start near the screen center"))
            return;
          const double scale = double(image->width()) / window->width();
          if (!check(image->pixelColor(qRound(10 * scale), qRound(100 * scale))
                             .alpha() == 0,
                     "3D background must be transparent"))
            return;
          if (!check(!window->mask().contains(QPoint(10, 100)),
                     "3D background clicks must pass through"))
            return;
          const QPoint center(window->width() / 2, window->height() / 2);
          QTest::mousePress(window, Qt::LeftButton, Qt::NoModifier, center);
          for (int step = 1; step <= 10; ++step)
            QTest::mouseMove(window, center + QPoint(step * 10, step * 3), 10);
          QTest::mouseRelease(window, Qt::LeftButton, Qt::NoModifier,
                              center + QPoint(100, 30));
          *stage = 2;
        } else if (*stage == 2) {
          const auto current = window->grabWindow();
          if (current == *image)
            return;
          *image = current;
          *region = window->mask();
          const QPointF center(window->width() / 2, window->height() / 2);
          QWheelEvent wheel(center, center, QPoint(), QPoint(0, 120),
                            Qt::NoButton, Qt::NoModifier, Qt::NoScrollPhase,
                            false);
          QCoreApplication::sendEvent(window, &wheel);
          *stage = 3;
        } else if (*stage == 3) {
          // WebEngine painting and the QML input-region poll complete
          // independently. Wait for both instead of assuming one timer tick
          // establishes that zoom has reached the compositor.
          if (window->grabWindow() == *image || window->mask() == *region)
            return;
          *image = window->grabWindow();
          *region = window->mask();
          const QPoint center(window->width() / 2, window->height() / 2);
          QTest::mousePress(window, Qt::RightButton, Qt::NoModifier, center);
          for (int step = 1; step <= 10; ++step)
            QTest::mouseMove(window, center + QPoint(step * 10, 0), 10);
          QTest::mouseRelease(window, Qt::RightButton, Qt::NoModifier,
                              center + QPoint(100, 0));
          *stage = 4;
        } else if (*stage == 4) {
          if (window->grabWindow() == *image || window->mask() == *region)
            return;
          if (!check(window->mask().contains(QPoint(window->width() - 138, 25)),
                     "Reset must remain reachable after pan"))
            return;
          QTest::mouseClick(window, Qt::LeftButton, Qt::NoModifier,
                            QPoint(window->width() - 138, 25));
          *stage = 5;
        } else if (*stage == 5) {
          if (window->mask() != *initialRegion || window->grabWindow() == *image)
            return;
          QTest::mouseClick(window, Qt::LeftButton, Qt::NoModifier,
                            QPoint(window->width() - 50, 25));
          *stage = 6;
        } else if (*stage == 6) {
          if (window->isVisible())
            return;
          backend.request(
              R"({"action":"show","scene":{"kind":"model3d","parts":[{"primitive":"sphere","position":[0,0,0],"rotation":[0,0,0],"scale":[2,2,2],"color":"#ff3366"}]}})");
          *stage = 7;
        } else if (*stage == 7) {
          const auto shot = window->grabWindow();
          if (shot.isNull() ||
              shot.pixelColor(shot.width() / 2, shot.height() / 2).alpha() == 0 ||
              !window->mask().contains(
                  QPoint(window->width() / 2, window->height() / 2)))
            return;
          QTest::mouseClick(window, Qt::LeftButton, Qt::NoModifier,
                            QPoint(window->width() - 50, 25));
          *stage = 8;
        } else {
          if (window->isVisible())
            return;
          qInfo("Native overlay tests passed: pixels, transparency, input "
                "regions, "
                "all primitives, orbit, zoom, pan, reset, close, reopen");
          app.exit(0);
        }
      });
  timer->start();
  QTimer::singleShot(15000, &app, [&app, &backend, stage, initialRegion] {
    qCritical("Native overlay test timed out at stage %d "
              "(0: annotations, 1: 3D, 2: orbit, 3: zoom/input region, "
              "4: pan/input region, 5: reset, 6: close, 7: reopen, 8: close)", *stage);
    qCritical() << "Current mask:" << backend.window->mask()
                << "Initial mask:" << *initialRegion
                << "Mouse buttons:" << QGuiApplication::mouseButtons()
                << "Visible:" << backend.window->isVisible()
                << "Reset visible:" << backend.window->property("canReset");
    app.exit(1);
  });
}
#endif

int main(int argc, char **argv) {
  QtWebEngineQuick::initialize();
  QGuiApplication app(argc, argv);
  app.setDesktopFileName("grok-overlay");
  app.setQuitOnLastWindowClosed(false);
  if (QGuiApplication::platformName() != "wayland") {
    std::puts("{\"error\":\"Desktop overlays require a Wayland session\"}");
    return 1;
  }
  Backend backend;
  QQmlApplicationEngine engine;
  engine.rootContext()->setContextProperty("backend", &backend);
  engine.load(QUrl("qrc:/Main.qml"));
  if (engine.rootObjects().isEmpty()) {
    std::puts("{\"error\":\"Cannot load Qt WebEngine overlay UI\"}");
    return 1;
  }
  backend.window = qobject_cast<QQuickWindow *>(engine.rootObjects().first());
  if (!backend.window)
    return 1;
  auto layer = LayerShellQt::Window::get(backend.window);
  layer->setScope("grok-overlay");
  layer->setLayer(LayerShellQt::Window::LayerOverlay);
  layer->setAnchors(
      LayerShellQt::Window::Anchors(LayerShellQt::Window::AnchorTop) |
      LayerShellQt::Window::AnchorBottom | LayerShellQt::Window::AnchorLeft |
      LayerShellQt::Window::AnchorRight);
  layer->setExclusiveZone(-1);
  layer->setActivateOnShow(false);
  QObject::connect(&app, &QGuiApplication::screenRemoved, &backend,
                   [&backend](QScreen *) { backend.dismiss(); });
#ifdef OVERLAY_SELF_TEST
  testOverlay(backend, app);
  return app.exec();
#endif
  QByteArray buffer;
  fcntl(STDIN_FILENO, F_SETFL, fcntl(STDIN_FILENO, F_GETFL) | O_NONBLOCK);
  QSocketNotifier reader(STDIN_FILENO, QSocketNotifier::Read);
  QObject::connect(&reader, &QSocketNotifier::activated, &backend, [&] {
    char chunk[8192];
    const auto count = ::read(STDIN_FILENO, chunk, sizeof(chunk));
    if (count == 0) {
      app.quit();
      return;
    }
    if (count < 0)
      return;
    buffer.append(chunk, count);
    if (buffer.size() > 1024 * 1024) {
      backend.fail("Overlay request exceeds 1 MiB");
      app.quit();
      return;
    }
    qsizetype newline;
    while ((newline = buffer.indexOf('\n')) >= 0) {
      const auto line = buffer.left(newline);
      buffer.remove(0, newline + 1);
      backend.request(line);
    }
  });
  return app.exec();
}
#include "main.moc"
