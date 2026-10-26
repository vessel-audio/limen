#include <algorithm>
#include <cstddef>
#include <memory>
#include <string>

#include <fcitx-module/dbus/dbus_public.h>
#include <fcitx-utils/dbus/bus.h>
#include <fcitx-utils/dbus/objectvtable.h>
#include <fcitx-utils/event.h>
#include <fcitx-utils/log.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/instance.h>

namespace {

constexpr char OBJECT_PATH[] = "/org/freedesktop/Fcitx5/VoiceInput";
constexpr char INTERFACE[] = "org.fcitx.Fcitx5.VoiceInput1";
constexpr std::size_t MAX_COMMIT_TEXT_BYTES = 16 * 1024;

class VoiceInputBridge final : public fcitx::AddonInstance,
                               public fcitx::dbus::ObjectVTable<VoiceInputBridge> {
public:
    explicit VoiceInputBridge(fcitx::Instance *instance) : instance_(instance) {
        deferred_registration_ = instance_->eventLoop().addDeferEvent(
            [this](fcitx::EventSource *) {
                registerOnBus();
                return false;
            });
    }

    bool CommitText(const std::string &text) {
        if (text.empty() || text.size() > MAX_COMMIT_TEXT_BYTES ||
            std::any_of(text.begin(), text.end(), [](unsigned char byte) {
                return byte < 0x20 || byte == 0x7f;
            })) {
            return false;
        }

        auto *input_context =
            instance_->inputContextManager().lastFocusedInputContext();
        if (input_context == nullptr || !input_context->hasFocus()) {
            return false;
        }

        input_context->commitString(text);
        return true;
    }

private:
    void registerOnBus() {
        auto *dbus_addon = instance_->addonManager().addon("dbus", true);
        if (dbus_addon == nullptr) {
            FCITX_ERROR() << "Voice Input requires the Fcitx D-Bus addon";
            return;
        }

        auto *bus = dbus_addon->call<fcitx::IDBusModule::bus>();
        if (bus == nullptr ||
            !bus->addObjectVTable(OBJECT_PATH, INTERFACE, *this)) {
            FCITX_ERROR() << "Voice Input failed to register its D-Bus object";
        }
    }

    fcitx::Instance *instance_;
    std::unique_ptr<fcitx::EventSource> deferred_registration_;

    FCITX_OBJECT_VTABLE_METHOD(CommitText, "CommitText", "s", "b");
};

class VoiceInputBridgeFactory final : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new VoiceInputBridge(manager->instance());
    }
};

} // namespace

FCITX_ADDON_FACTORY(VoiceInputBridgeFactory)
