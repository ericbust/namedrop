SELECT EXISTS(SELECT 1 FROM coordinations c
             WHERE c.session_socket_id=?1 AND c.worktree_id=?2 AND c.coordination_id=?3
               AND ((c.outcome IN ('blocked','cancelled') AND c.report IS NOT NULL
                   AND EXISTS(SELECT 1 FROM mailbox_deliveries d
                       WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                         AND d.message_id=c.report_message_id AND d.state='settled'))
                 OR (c.outcome='completed' AND c.report IS NOT NULL
                   AND EXISTS(SELECT 1 FROM mailbox_deliveries d
                       WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                         AND d.message_id=c.report_message_id AND d.state='cancelled'))
                 OR (c.outcome='open' AND EXISTS(SELECT 1 FROM pane_addresses p
                       WHERE p.session_socket_id=c.session_socket_id AND p.worktree_id=c.worktree_id
                         AND p.address_id=c.owner_address_id AND p.retired=1)))
               AND NOT EXISTS(SELECT 1 FROM mailbox_deliveries d
                   JOIN delivery_seats s USING(session_socket_id,worktree_id,delivery_id)
                   WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                     AND s.progress NOT IN ('settled','failed','cancelled')
                     AND (d.message_id=c.report_message_id OR d.message_id=c.wake_message_id
                       OR d.message_id IN (SELECT l.message_id FROM coordination_links l
                         JOIN coordination_assignments a USING(session_socket_id,worktree_id,assignment_id)
                         WHERE a.session_socket_id=c.session_socket_id AND a.worktree_id=c.worktree_id
                           AND a.coordination_id=c.coordination_id))))
