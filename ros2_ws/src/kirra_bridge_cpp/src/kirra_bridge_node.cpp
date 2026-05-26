#include "rclcpp/rclcpp.hpp"
#include "geometry_msgs/msg/twist.hpp"
#include "std_msgs/msg/string.hpp"
#include "std_msgs/msg/u_int32.hpp"
#include "kirra.h"

#include <cmath>
#include <functional>
#include <string>

class KirraFirewallNode : public rclcpp::Node {
public:
    KirraFirewallNode() : Node("kirra_firewall_node") {
        auto qos_volatile = rclcpp::QoS(rclcpp::KeepLast(1)).reliable().durability_volatile();
        auto qos_transient = rclcpp::QoS(rclcpp::KeepLast(1)).reliable().transient_local();

        safe_publisher_     = this->create_publisher<geometry_msgs::msg::Twist>("/kirra/cmd_vel_safe", qos_volatile);
        state_publisher_    = this->create_publisher<std_msgs::msg::String>("/kirra/system_state", qos_transient);
        audit_publisher_    = this->create_publisher<std_msgs::msg::String>("/kirra/audit_events", qos_volatile);
        trust_publisher_    = this->create_publisher<std_msgs::msg::UInt32>("/kirra/metrics/trust_score", qos_volatile);
        mutation_publisher_ = this->create_publisher<std_msgs::msg::UInt32>("/kirra/metrics/mutation_count", qos_volatile);

        raw_subscriber_ = this->create_subscription<geometry_msgs::msg::Twist>(
            "/cmd_vel",
            qos_volatile,
            std::bind(&KirraFirewallNode::handle_incoming_trajectory, this, std::placeholders::_1)
        );

        mutation_count_  = 0;
        current_posture_ = "Normal";

        publish_system_state(current_posture_);
    }

private:
    void handle_incoming_trajectory(const geometry_msgs::msg::Twist::SharedPtr raw_msg) {
        constexpr double DT = 0.050;
        uint32_t active_trust = kirra_get_trust_score();

        if (active_trust < 45 || current_posture_ == "LockedOut") {
            force_hard_lockout(raw_msg->linear.x);
            return;
        }

        double sanitized_linear_x  = kirra_filter_move_velocity(raw_msg->linear.x, DT);
        double sanitized_angular_z = kirra_filter_rotate_velocity(raw_msg->angular.z, DT);
        uint32_t post_filter_trust = kirra_get_trust_score();

        if (post_filter_trust < 45) {
            force_hard_lockout(raw_msg->linear.x);
            return;
        }

        auto safe_msg = geometry_msgs::msg::Twist();
        safe_msg.linear.x  = sanitized_linear_x;
        safe_msg.angular.z = sanitized_angular_z;

        bool is_mutated = (std::abs(sanitized_linear_x  - raw_msg->linear.x)  > 0.001 ||
                           std::abs(sanitized_angular_z - raw_msg->angular.z) > 0.001);

        if (is_mutated) {
            mutation_count_++;
            std::string new_posture = (post_filter_trust < 85) ? "Degraded" : "Normal";
            if (new_posture != current_posture_) {
                current_posture_ = new_posture;
                publish_system_state(current_posture_);
            }
            publish_audit_log("TRAJECTORY_MUTATED",   raw_msg->linear.x, sanitized_linear_x, post_filter_trust, "Contract Breach Rectified.");
        } else {
            publish_audit_log("TRAJECTORY_APPROVED", raw_msg->linear.x, sanitized_linear_x, post_filter_trust, "Nominal.");
        }

        auto trust_msg = std_msgs::msg::UInt32(); trust_msg.data = post_filter_trust;  trust_publisher_->publish(trust_msg);
        auto mut_msg   = std_msgs::msg::UInt32(); mut_msg.data   = mutation_count_;    mutation_publisher_->publish(mut_msg);

        safe_publisher_->publish(safe_msg);
    }

    void force_hard_lockout(double raw_val) {
        if (current_posture_ != "LockedOut") {
            current_posture_ = "LockedOut";
            publish_system_state(current_posture_);
        }
        auto safe_msg = geometry_msgs::msg::Twist();
        safe_msg.linear.x  = 0.0;
        safe_msg.angular.z = 0.0;
        safe_publisher_->publish(safe_msg);
        publish_audit_log("HARD_FAILSAFE_ACTIVE", raw_val, 0.0, kirra_get_trust_score(), "CRITICAL_LOCKOUT");
    }

    void publish_system_state(const std::string & state) {
        auto msg = std_msgs::msg::String();
        msg.data = "POSTURE_STATE=" + state;
        state_publisher_->publish(msg);
    }

    void publish_audit_log(const std::string & e, double r, double s, uint32_t t, const std::string & m) {
        auto msg = std_msgs::msg::String();
        msg.data = "[KIRRA AUDIT] EVENT=" + e + " | RAW=" + std::to_string(r)
                 + " | SAFE=" + std::to_string(s) + " | SCORE=" + std::to_string(t) + " | MSG=" + m;
        audit_publisher_->publish(msg);
    }

    rclcpp::Publisher<geometry_msgs::msg::Twist>::SharedPtr    safe_publisher_;
    rclcpp::Publisher<std_msgs::msg::String>::SharedPtr        state_publisher_;
    rclcpp::Publisher<std_msgs::msg::String>::SharedPtr        audit_publisher_;
    rclcpp::Publisher<std_msgs::msg::UInt32>::SharedPtr        trust_publisher_;
    rclcpp::Publisher<std_msgs::msg::UInt32>::SharedPtr        mutation_publisher_;
    rclcpp::Subscription<geometry_msgs::msg::Twist>::SharedPtr raw_subscriber_;
    uint32_t    mutation_count_;
    std::string current_posture_;
};

int main(int argc, char **argv) {
    rclcpp::init(argc, argv);
    rclcpp::spin(std::make_shared<KirraFirewallNode>());
    rclcpp::shutdown();
    return 0;
}
