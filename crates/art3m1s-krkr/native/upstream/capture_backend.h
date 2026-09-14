#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include "backend/SWRenderBackend.h"
#include "art3m1s_krkr.h"

namespace art3m1s::krkr
{
struct FrameView
{
    const uint8_t* pixels = nullptr;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t stride = 0;
    uint64_t frame_id = 0;
    uint64_t generation = 0;
};

class CaptureBackend final : public krkrsdl3::iTVPRenderBackend
{
public:
    explicit CaptureBackend(const Art3m1sKrkrRenderHostV1* render_host = nullptr);
    ~CaptureBackend() override;

    const char* GetName() const override { return "art3m1s-capture"; }
    bool IsHardware() const override { return false; }

    void BeginFrame(int winWidth, int winHeight) override;
    void EndFrame() override;

    void* CreateWindowTexture(int width, int height) override;
    void UpdateWindowTexture(
        void* handle, const uint8_t* buff, int width, int height, int pitch) override;
    void DestroyWindowTexture(void* handle) override;
    void DrawWindowTexture(void* handle, float posX, float posY, float width, float height) override;

    void* CreateTarget(int width, int height) override;
    void DestroyTarget(void* target) override;
    void SetTarget(void* target) override;
    void ClearTarget(bool clearColor) override;
    uint8_t* LockTarget(void* target, int& pitch) override;
    void UnlockTarget(void* target) override;
    void* GetTargetTexture(void* target) override;
    void UpdateTargetTexture(
        void* target, const uint8_t* pixels, int width, int height, int pitch) override;
    void* CreateTexture(int width, int height) override;
    void UpdateTexture(
        void* texture, const uint8_t* pixels, int width, int height, int pitch) override;
    void DestroyTexture(void* texture) override;
    uint8_t* LockTexture(void* texture, int& pitch) override;
    void SetMask(void* maskTarget) override;
    void SetBlendMode(int mode, const float* uniformColor) override;
    void DrawMesh(const float* vertices,
                  int vertexCount,
                  const uint16_t* indices,
                  int indexCount,
                  void* texture,
                  float opacity,
                  const float* colorModulation) override;

    void LayerSetBlend(int method, float opacity, const float* uniformColor) override;
    void LayerDrawRect(void* texture,
                       float x,
                       float y,
                       float w,
                       float h,
                       float u0,
                       float v0,
                       float u1,
                       float v1) override;

    FrameView AcquireFrame() const;
    bool HostFailed() const { return host_failed_; }
    bool UsesRenderHost() const { return render_host_.user_data != nullptr; }

private:
    struct ShadowTexture
    {
        uint32_t width = 0;
        uint32_t height = 0;
        std::vector<uint8_t> pixels;
    };

    struct WindowTexture
    {
        uint32_t width = 0;
        uint32_t height = 0;
        uint64_t host_texture = 0;
        std::vector<uint8_t> pixels;
    };

    WindowTexture* FindWindowTexture(void* handle) const;
    ShadowTexture* FindShadowTexture(void* handle) const;

    krkrsdl3::SWRenderBackend software_;
    std::unordered_set<WindowTexture*> window_textures_;
    std::unordered_map<void*, std::unique_ptr<ShadowTexture>> shadow_textures_;
    std::vector<uint8_t> canvas_;
    std::vector<uint8_t> published_;
    uint32_t width_ = 0;
    uint32_t height_ = 0;
    uint32_t published_width_ = 0;
    uint32_t published_height_ = 0;
    uint64_t frame_id_ = 0;
    uint64_t generation_ = 0;
    Art3m1sKrkrRenderHostV1 render_host_{};
    bool host_failed_ = false;
};
} // namespace art3m1s::krkr
