��|�   SE Linux!         	   @   @                 @                               cap       setfcap   	   setpcap      fowner      sys_boot      sys_tty_config      net_raw	      sys_admin
      sys_chroot
      sys_module	      sys_rawio      dac_override	      ipc_owner      kill      dac_read_search	      sys_pacct      net_broadcast      net_bind_service      sys_nice      sys_time      fsetid      mknod      setgid      setuid      lease	      net_admin      audit_write   
   linux_immutable
      sys_ptrace      audit_control      ipc_lock      sys_resource      chown            cap2      mac_override	      mac_admin
      audit_read      syslog      block_suspend
      wake_alarm            x_device      get_property      list_property      set_property      add      setfocus      create   
   freeze      getfocus      remove      write   	   force_cursor      destroy      bell      getattr      grab      setattr      read      manage      use            database      drop      create      relabelfrom      getattr      setattr	      relabelto      	   	   ipc	      associate      create      write	      unix_read      destroy      getattr      setattr      read
   	   unix_write            socket      map   
   append      bind      connect      create      write      relabelfrom      ioctl	      name_bind      sendto      getattr      setattr      accept      getopt      read      setopt      shutdown      recvfrom      lock	   	   relabelto      listen            file      map   
   append      create      execute      write      relabelfrom      link      unlink      ioctl      watch_with_perm      audit_access      watch_reads      getattr      setattr      execmod      read      rename      watch_sb      watch_mount      watch      lock	   	   relabelto      mounton      open      quotaon`   `   
      X             tcp_socketsocket	      node_bind      name_connect                          +   
          msgqipc   
   enqueue                                        fd      use                           J             process2      nosuid_transition      nnp_transition                    
      &              llc_socketsocket                          !              iucv_socketsocket                          \              unix_dgram_socketsocket                          =             netlink_xfrm_socketsocket      nlmsg_write
      nlmsg_read                          0              netlink_dnrt_socketsocket                          2              netlink_generic_socketsocket                                        ax25_socketsocket                          B             obsolete_netlink_ip6fw_socketsocket      nlmsg_write
      nlmsg_read                    
      _              x25_socketsocket                    
      Z             tun_socketsocket      attach_queue                    
      [             udp_socketsocket	      node_bind                           (             lockdown	      integrity      confidentiality                          1              netlink_fib_lookup_socketsocket                          	              bluetooth_socketsocket                          -             netlink_audit_socketsocket      nlmsg_relay      nlmsg_tty_audit      nlmsg_readpriv      nlmsg_write
      nlmsg_read                          N              rose_socketsocket                                        binder      impersonate      transfer      call      set_context_mgr                           E             peer      recv                                        blk_filefile                                        chr_filefile                          '              lnk_filefile                    
      ?              nfc_socketsocket                           C             packet      forward_out      send      recv
      forward_in	      relabelto                          V              socketsocket                                       filefile
      entrypoint      execute_no_trans                           @             node      sendto      recvfrom                                        decnet_socketsocket                          G              phonet_socketsocket                          6              netlink_nflog_socketsocket                           $             key      create      write      view      link      setattr      read      search                          <             netlink_tcpdiag_socketsocket      nlmsg_write
      nlmsg_read                          ]             unix_stream_socketsocket	      connectto                          8             netlink_route_socketsocket      nlmsg_write
      nlmsg_read                                        infiniband_endport      manage_subnet                          7              netlink_rdma_socketsocket                          ^              vsock_socketsocket                    
          
   
      filesystem	      associate   	   quotaget      relabelfrom      getattr      quotamod      mount      remount      unmount   
   watch	      relabelto                                                              L             rawip_socketsocket	      node_bind                          R   	           semipc                           W             system      stop   	   status      module_request      reboot      disable      enable      module_load      ipc_info      syslog_read      halt      reload   
   start      syslog_console
      syslog_mod                           Q             security      compute_member      compute_user      compute_create
      setenforce      check_context      setcheckreqprot      validate_trans      compute_relabel   	   setbool      load_policy      read_policy   
   setsecparam
      compute_av                          Y              tipc_socketsocket                    
                    ipx_socketsocket                           I             process      getcap      setcap      sigstop      sigchld	      getrlimit      share      execheap
      setcurrent      setfscreate      setkeycreate      siginh      dyntransition
      transition      fork
      getsession
      noatsecure      sigkill      signull	      setrlimit      getattr   	   getsched      setexec   
   setsched      getpgid      setpgid      ptrace	      execstack	      rlimitinh      setsockcreate      signal      execmem                                        capability2cap2                    
                     cap_usernscap                    	                    fifo_filefile                    
      `              xdp_socketsocket                          3              netlink_iscsi_socketsocket                          H              pppox_socketsocket                          :              netlink_selinux_socketsocket                    	      U              sock_filefile                                        atmpvc_socketsocket                          9              netlink_scsitransport_socketsocket                           *             msg      send      receive                                        appletalk_socketsocket                                        association
      setcontext      sendto      recvfrom      polmatch                                        caif_socketsocket                          ;              netlink_socketsocket                                        infiniband_pkey      access                    
                    alg_socketsocket                                       dirfile      rmdir      remove_name      add_name      reparent      search                             	           ipcipc                          .              netlink_connector_socketsocket                                        atmsvc_socketsocket                           
             bpf	      prog_load	      map_write      map_read
      map_create      prog_run                                        irda_socketsocket                    
      M              rds_socketsocket                          P             sctp_socketsocket	      node_bind      name_connect      association                          5              netlink_netfilter_socketsocket                           #             kernel_service      create_files_as      use_as_override                                        ieee802154_socketsocket                          >              netrom_socketsocket                          S   
          shmipc   
   lock                    
                     capabilitycap                                        cap2_usernscap2                                       dccp_socketsocket	      node_bind      name_connect                    
      "              kcm_socketsocket                          4              netlink_kobject_uevent_socketsocket                          O              rxrpc_socketsocket                    
                    can_socketsocket                                         isdn_socketsocket                    
      %              key_socketsocket                           ,             netif      egress      ingress                           A             obsolete_netlink_firewall_socketsocket      nlmsg_write
      nlmsg_read                          D              packet_socketsocket                    
       F             perf_event
      tracepoint      write      read      cpu      kernel      open                    
       )             memprotect	      mmap_zero                          K              qipcrtr_socketsocket                          /              netlink_crypto_socketsocket                                       icmp_socketsocket	      node_bind                    
      T              smc_socketsocket                                    object_r@           @                     source_r@   @                 @   @                           target_r@   @                 @   @          @                 unconfined_r@   @                 @   @                                   unlabeled_t             runcon_target_a             file_like_a             devpts_t             source_t             hermetic_bin_t             target_t             lib_t   	          domain_a   
          selinuxfs_t             unconfined_a             unconfined_t             transition_t                source_u@   @                          @           @   @                    @                     system_u@   @                          @           @   @                    @                     target_u@   @                          @           @   @                    @                     unconfined_u@   @                          @           @   @                    @                           xserver_object_manager             s0   @   @                        s1   @   @                        s2   @   @                                 c0          c1          c2d     9  ����  *  ����    ����  0  ����    ����    ����  N  ����    ����  `  ����  @  ����    ����  "  ����  [  ����    ����    ����  _  ����  M  ����  B  ����  ?  ����  C  ����    ����  ^  ����    ����  ,  ����  F  ����  
  ����  I  ����  3  ����    ����  J  ����  G  ����  <  ����    ����  (  ����  =  ����  $  ����  H  ����    ����  )  ����    ����  4  ����  /  ����    ����     ����    ����  &  ����  S  ����         :  ����  \  ����    ����  5  ����  D  ����  P  ����  E  ����  1  ����  U  ����    ����    ����    ����  ;  ����    ����  R  ����  6  ����    ����  I       #  ����  -  ����  T  ����    ����    ����  7  ����  Q  ����  ]  ����    ����    ����  W  ����  	  ����  +  ����  A  ����  L  ����         %  ����  .  ����  '  ����    ����    ����  2  ����  O  ����	         >  ����  V  ����  K  ����  Z  ����    ����  X  ����  Y  ����  8  ����    ����  !  ����                                     @                             @                             @                             @                             @                             @                             @                             @                             @                             @                             @                             @                             @                             @                             @                             @                             @           
                  @           	                  @                             @                             @                             @                             @                             @                             @                             @                             @                                    sockfs               @                 pipefs               @                 tmpfs               @                 shm               @                 mqueue               @              	   hugetlbfs               @                 devpts               @                 xfs               @                 reiserfs               @                 jfs               @                 jffs2               @                 ext4               @                 ext3               @                 ext2               @                             cgroup      /                   @              cgroup2      /                   @              debugfs      /                   @              proc      /                   @              pstore      /                   @           	   selinuxfs      /          
         @              sysfs      /                   @              tracefs      /                   @               @   @                @   @                 @   @                 @   @                 @   @                 @   @          $       @   @          @       @   @          �       @   @                 @   @                @   @                 @   @                @   @                 