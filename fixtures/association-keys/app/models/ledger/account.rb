module Ledger
  class Account < ApplicationRecord
    has_many :tickets
  end
end
