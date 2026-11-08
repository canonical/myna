// SPDX-FileCopyrightText: 2026 Myna contributors
// SPDX-License-Identifier: AGPL-3.0-or-later

#include <algorithm>
#include <array>
#include <cerrno>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>
#include <utility>
#include <vector>

#include <fcitx-utils/event.h>
#include <fcitx-utils/handlertable.h>
#include <fcitx-utils/log.h>
#include <fcitx-utils/utf8.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include <fcitx/userinterface.h>

namespace {

constexpr uint8_t kHello = 'H';
constexpr uint8_t kAcquire = 'A';
constexpr uint8_t kCommit = 'C';
constexpr uint8_t kPreedit = 'P';
constexpr uint8_t kRelease = 'R';

constexpr uint8_t kReady = 'V';
constexpr uint8_t kAcquired = 'Y';
constexpr uint8_t kCommitted = 'K';
constexpr uint8_t kNoTarget = 'N';
constexpr uint8_t kFocusLost = 'F';
constexpr uint8_t kTargetGone = 'G';
constexpr uint8_t kError = 'E';

constexpr uint8_t kAcquiredHasBefore = 1 << 0;
constexpr uint32_t kProtocolVersion = 1;
constexpr uint64_t kFocusLossGraceUsec = 500000;

constexpr size_t kMaxCommitBytes = 1024 * 1024;

std::filesystem::path socketPath() {
    if (const char *overridePath = std::getenv("MYNA_INPUT_METHOD_SOCKET");
        overridePath != nullptr && overridePath[0] != '\0') {
        return overridePath;
    }

    if (const char *directory = std::getenv("MYNA_INPUT_METHOD_DIR");
        directory != nullptr && directory[0] != '\0') {
        return std::filesystem::path(directory) / "fcitx5.sock";
    }

    const char *runtime = std::getenv("XDG_RUNTIME_DIR");
    if (runtime != nullptr && runtime[0] != '\0') {
        return std::filesystem::path(runtime) / "snap.myna" /
               "myna-input-methods" / "fcitx5.sock";
    }
    return std::filesystem::path("/tmp") / "myna-input-methods" /
           "fcitx5.sock";
}

void appendU32(std::vector<uint8_t> &frame, uint32_t value) {
    frame.push_back(static_cast<uint8_t>((value >> 24) & 0xff));
    frame.push_back(static_cast<uint8_t>((value >> 16) & 0xff));
    frame.push_back(static_cast<uint8_t>((value >> 8) & 0xff));
    frame.push_back(static_cast<uint8_t>(value & 0xff));
}

uint32_t readU32(const uint8_t *data) {
    return (static_cast<uint32_t>(data[0]) << 24) |
           (static_cast<uint32_t>(data[1]) << 16) |
           (static_cast<uint32_t>(data[2]) << 8) |
           static_cast<uint32_t>(data[3]);
}

} // namespace

class MynaFcitx final : public fcitx::AddonInstance {
public:
    explicit MynaFcitx(fcitx::Instance *instance)
        : instance_(instance), path_(socketPath()) {
        handlers_.emplace_back(instance_->watchEvent(
            fcitx::EventType::InputContextFocusIn,
            fcitx::EventWatcherPhase::PostInputMethod,
            [this](fcitx::Event &event) {
                if (auto *contextEvent =
                        dynamic_cast<fcitx::InputContextEvent *>(&event)) {
                    focusArrived(contextEvent->inputContext());
                }
            }));
        handlers_.emplace_back(instance_->watchEvent(
            fcitx::EventType::InputContextFocusOut,
            fcitx::EventWatcherPhase::PostInputMethod,
            [this](fcitx::Event &event) {
                if (auto *contextEvent =
                        dynamic_cast<fcitx::InputContextEvent *>(&event)) {
                    scheduleFocusLoss(contextEvent->inputContext());
                }
            }));
        handlers_.emplace_back(instance_->watchEvent(
            fcitx::EventType::InputContextDestroyed,
            fcitx::EventWatcherPhase::PostInputMethod,
            [this](fcitx::Event &event) {
                if (auto *contextEvent =
                        dynamic_cast<fcitx::InputContextEvent *>(&event)) {
                    lostTarget(contextEvent->inputContext(), kTargetGone);
                }
            }));
        listen();
    }

    ~MynaFcitx() override {
        pendingLossEvent_.reset();
        closeClient();
        listenerEvent_.reset();
        if (listener_ >= 0) {
            ::close(listener_);
        }
        std::error_code error;
        std::filesystem::remove(path_, error);
    }

private:
    struct Client {
        int fd = -1;
        bool hello = false;
        fcitx::InputContext *target = nullptr;
        std::vector<uint8_t> input;
        std::unique_ptr<fcitx::EventSourceIO> event;
    };

    void listen() {
        std::error_code error;
        std::filesystem::create_directories(path_.parent_path(), error);
        if (error) {
            FCITX_LOG(Error)
                << "myna-fcitx5: cannot create " << path_.parent_path()
                << ": " << error.message();
            return;
        }

        std::filesystem::remove(path_, error);
        listener_ = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
        if (listener_ < 0) {
            FCITX_LOG(Error) << "myna-fcitx5: socket: " << std::strerror(errno);
            return;
        }

        sockaddr_un address{};
        address.sun_family = AF_UNIX;
        const std::string path = path_.string();
        if (path.size() >= sizeof(address.sun_path)) {
            FCITX_LOG(Error) << "myna-fcitx5: socket path is too long: " << path;
            return;
        }
        std::memcpy(address.sun_path, path.c_str(), path.size() + 1);
        if (::bind(listener_, reinterpret_cast<sockaddr *>(&address), sizeof(address)) < 0) {
            FCITX_LOG(Error) << "myna-fcitx5: bind " << path << ": "
                             << std::strerror(errno);
            return;
        }
        ::chmod(path.c_str(), 0600);
        if (::listen(listener_, 4) < 0) {
            FCITX_LOG(Error) << "myna-fcitx5: listen " << path << ": "
                             << std::strerror(errno);
            return;
        }

        listenerEvent_ = instance_->eventLoop().addIOEvent(
            listener_, fcitx::IOEventFlag::In,
            [this](fcitx::EventSourceIO *, int, fcitx::IOEventFlags) {
                acceptClient();
                return true;
            });
        FCITX_LOG(Info) << "myna-fcitx5: listening on " << path;
    }

    void acceptClient() {
        for (;;) {
            const int fd =
                ::accept4(listener_, nullptr, nullptr, SOCK_CLOEXEC | SOCK_NONBLOCK);
            if (fd < 0) {
                if (errno != EAGAIN && errno != EWOULDBLOCK) {
                    FCITX_LOG(Error)
                        << "myna-fcitx5: accept: " << std::strerror(errno);
                }
                return;
            }

            closeClient();
            client_.fd = fd;
            client_.event = instance_->eventLoop().addIOEvent(
                fd, fcitx::IOEventFlag::In,
                [this](fcitx::EventSourceIO *, int, fcitx::IOEventFlags) {
                    readClient();
                    return true;
                });
        }
    }

    void closeClient() {
        clearPreedit();
        client_.event.reset();
        if (client_.fd >= 0) {
            ::close(client_.fd);
        }
        client_ = {};
    }

    bool sendFrame(const std::vector<uint8_t> &frame) {
        size_t sent = 0;
        while (sent < frame.size()) {
            const ssize_t n = ::send(client_.fd, frame.data() + sent,
                                      frame.size() - sent, MSG_NOSIGNAL);
            if (n > 0) {
                sent += static_cast<size_t>(n);
                continue;
            }
            if (n < 0 && errno == EINTR) {
                continue;
            }
            if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
                return false;
            }
            return false;
        }
        return true;
    }

    void sendTag(uint8_t tag) {
        const std::vector<uint8_t> frame{tag};
        if (client_.fd >= 0) {
            sendFrame(frame);
        }
    }

    void sendError(const std::string &message) {
        std::vector<uint8_t> frame{kError};
        appendU32(frame, static_cast<uint32_t>(message.size()));
        frame.insert(frame.end(), message.begin(), message.end());
        sendFrame(frame);
    }

    void readClient() {
        std::array<uint8_t, 4096> buffer{};
        for (;;) {
            const ssize_t n = ::recv(client_.fd, buffer.data(), buffer.size(), 0);
            if (n > 0) {
                client_.input.insert(client_.input.end(), buffer.begin(),
                                     buffer.begin() + n);
                continue;
            }
            if (n == 0) {
                closeClient();
                return;
            }
            if (errno == EINTR) {
                continue;
            }
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                break;
            }
            closeClient();
            return;
        }

        while (!client_.input.empty()) {
            const uint8_t tag = client_.input.front();
            size_t frameSize = 0;
            if (tag == kHello || tag == kAcquire || tag == kRelease) {
                frameSize = 1;
            } else if (tag == kCommit || tag == kPreedit) {
                if (client_.input.size() < 5) {
                    return;
                }
                const uint32_t length = readU32(client_.input.data() + 1);
                if (length > kMaxCommitBytes) {
                    sendError("commit exceeds the maximum size");
                    closeClient();
                    return;
                }
                frameSize = 5 + static_cast<size_t>(length);
                if (client_.input.size() < frameSize) {
                    return;
                }
            } else {
                sendError("unknown frame tag");
                closeClient();
                return;
            }

            if (tag == kHello) {
                client_.hello = true;
                std::vector<uint8_t> ready{kReady};
                appendU32(ready, kProtocolVersion);
                sendFrame(ready);
            } else if (!client_.hello) {
                sendError("hello required");
                closeClient();
                return;
            } else if (tag == kAcquire) {
                acquire();
            } else if (tag == kCommit) {
                commit(std::string(
                    reinterpret_cast<const char *>(client_.input.data() + 5),
                    readU32(client_.input.data() + 1)));
            } else if (tag == kPreedit) {
                setPreedit(std::string(
                    reinterpret_cast<const char *>(client_.input.data() + 5),
                    readU32(client_.input.data() + 1)));
            } else {
                clearPreedit();
                cancelFocusLoss();
                client_.target = nullptr;
                sendTag(kCommitted);
            }
            client_.input.erase(client_.input.begin(),
                                client_.input.begin() + frameSize);
        }
    }

    void acquire() {
        auto *target = instance_->lastFocusedInputContext();
        if (target == nullptr) {
            target = instance_->mostRecentInputContext();
        }
        if (target == nullptr ||
            target == instance_->inputContextManager().dummyInputContext()) {
            client_.target = nullptr;
            sendTag(kNoTarget);
            return;
        }
        client_.target = target;
        std::vector<uint8_t> payload{kAcquiredHasBefore};
        if (const auto before = characterBeforeCursor(target)) {
            payload.insert(payload.end(), before->begin(), before->end());
        } else {
            payload.front() = 0;
        }
        std::vector<uint8_t> frame{kAcquired};
        appendU32(frame, static_cast<uint32_t>(payload.size()));
        frame.insert(frame.end(), payload.begin(), payload.end());
        sendFrame(frame);
    }

    void commit(const std::string &text) {
        if (client_.target == nullptr || !client_.target->hasFocus()) {
            client_.target = nullptr;
            cancelFocusLoss();
            sendTag(kFocusLost);
            return;
        }
        clearPreedit();
        client_.target->commitString(text);
        sendTag(kCommitted);
    }

    void setPreedit(const std::string &text) {
        if (!applyPreedit(text)) {
            return;
        }
        sendTag(kCommitted);
    }

    bool applyPreedit(const std::string &text) {
        if (client_.target == nullptr || !client_.target->hasFocus()) {
            client_.target = nullptr;
            cancelFocusLoss();
            sendTag(kFocusLost);
            return false;
        }
        fcitx::Text preedit;
        preedit.append(text);
        client_.target->inputPanel().setPreedit(preedit);
        client_.target->updatePreedit();
        client_.target->updateUserInterface(
            fcitx::UserInterfaceComponent::InputPanel);
        return true;
    }

    void clearPreedit() {
        if (client_.target != nullptr) {
            applyPreedit({});
        }
    }

    static std::optional<std::string>
    characterBeforeCursor(fcitx::InputContext *target) {
        const auto &surrounding = target->surroundingText();
        if (!surrounding.isValid() || surrounding.cursor() == 0) {
            return std::nullopt;
        }
        const auto &text = surrounding.text();
        if (!fcitx::utf8::validate(text)) {
            return std::nullopt;
        }
        size_t end = fcitx::utf8::ncharByteLength(
            text.begin(), surrounding.cursor());
        if (end == 0 || end > text.size()) {
            return std::nullopt;
        }
        size_t begin = end;
        do {
            --begin;
        } while (begin > 0 &&
                 (static_cast<uint8_t>(text[begin]) & 0xc0) == 0x80);
        return text.substr(begin, end - begin);
    }

    void scheduleFocusLoss(fcitx::InputContext *context) {
        if (client_.target == nullptr || client_.target != context) {
            return;
        }
        pendingLossTarget_ = context;
        if (pendingLossEvent_) {
            if (pendingLossEvent_->isEnabled()) {
                return;
            }
            pendingLossEvent_.reset();
        }
        pendingLossEvent_ = instance_->eventLoop().addTimeEvent(
            CLOCK_MONOTONIC, fcitx::now(CLOCK_MONOTONIC) + kFocusLossGraceUsec,
            0, [this](fcitx::EventSourceTime *, uint64_t) {
                auto *target = pendingLossTarget_;
                pendingLossTarget_ = nullptr;
                if (target != nullptr && client_.target == target &&
                    !target->hasFocus()) {
                    client_.target = nullptr;
                    sendTag(kFocusLost);
                }
                return false;
            });
        pendingLossEvent_->setOneShot();
    }

    void focusArrived(fcitx::InputContext *context) {
        if (context != nullptr && pendingLossTarget_ == context) {
            cancelFocusLoss();
        }
        if (context != nullptr && client_.target != nullptr &&
            client_.target != context && pendingLossTarget_ == client_.target &&
            !client_.target->hasFocus()) {
            auto *lost = client_.target;
            cancelFocusLoss();
            client_.target = nullptr;
            if (lost != nullptr) {
                sendTag(kFocusLost);
            }
        }
    }

    void cancelFocusLoss() {
        pendingLossEvent_.reset();
        pendingLossTarget_ = nullptr;
    }

    void lostTarget(fcitx::InputContext *context, uint8_t reason) {
        if (client_.fd < 0 || client_.target == nullptr ||
            client_.target != context) {
            return;
        }
        cancelFocusLoss();
        client_.target = nullptr;
        sendTag(reason);
    }

    fcitx::Instance *instance_;
    const std::filesystem::path path_;
    int listener_ = -1;
    std::unique_ptr<fcitx::EventSourceIO> listenerEvent_;
    Client client_;
    fcitx::InputContext *pendingLossTarget_ = nullptr;
    std::unique_ptr<fcitx::EventSourceTime> pendingLossEvent_;
    std::vector<std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>>>
        handlers_;
};

class MynaFcitxFactory final : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new MynaFcitx(manager->instance());
    }
};

#ifdef MYNA_FCITX5_HAVE_ADDON_FACTORY_V2
FCITX_ADDON_FACTORY_V2(myna_fcitx5, MynaFcitxFactory);
#else
FCITX_ADDON_FACTORY(MynaFcitxFactory);
#endif
