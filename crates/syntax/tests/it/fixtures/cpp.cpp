#include <vector>
#include <string>

namespace app {

template <typename T>
class Stack {
public:
    void push(const T &value) { items_.push_back(value); }
    bool empty() const noexcept { return items_.empty(); }

private:
    std::vector<T> items_;
};

auto run(const std::string &name) -> int {
    Stack<int> stack;
    stack.push(42);
    if (name == "x" && !stack.empty()) {
        return 1;
    }
    return nullptr == nullptr ? 0 : 2;
}

}  // namespace app
